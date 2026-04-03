/**
 * E2EE Web Worker — AES-256-GCM encryption/decryption for encoded media frames.
 *
 * Frame format (v3 — KDF chain):
 *   [codec header (N bytes)] [KID 1 byte] [CTR 4 bytes big-endian] [ciphertext + GCM tag 16 bytes] [trailer 1 byte = N]
 *
 * Key improvements over v2:
 *   - KDF chain: per-frame forward secrecy — each frame derives its own message key
 *     then irreversibly advances the chain key. Compromising one frame key does not
 *     reveal past or future frame keys.
 *   - AES-256-GCM: industry-standard 256-bit symmetric encryption
 *   - Sender ID in AAD: binds the sender identity to each frame cryptographically,
 *     preventing cross-sender frame injection
 *   - 128-frame replay window (up from 64)
 *
 * AAD (additional authenticated data) = sender_id (UTF-8) + codec header + e2ee header (KID + CTR).
 */

// Type declarations for RTCEncodedFrame APIs available inside a Worker scope
interface RTCEncodedFrame {
  readonly timestamp: number
  data: ArrayBuffer
  getMetadata(): RTCEncodedFrameMetadata
}

interface RTCEncodedFrameMetadata {
  synchronizationSource?: number
  contributingSources?: number[]
  payloadType?: number
  sequenceNumber?: number
  [key: string]: unknown
}

interface RTCTransformEvent extends Event {
  readonly transformer: {
    readonly readable: ReadableStream<RTCEncodedFrame>
    readonly writable: WritableStream<RTCEncodedFrame>
    readonly options: TransformOptions
  }
}

interface TransformOptions {
  operation: 'encrypt' | 'decrypt'
  participantId?: string
  senderId?: string
}

// ── Key Chain Management ────────────────────────────────────────────────

/**
 * Per-sender chain state. Each sender key initialises a chain; every frame
 * advances it forward (KDF chain à la Signal Sender Keys).
 */
interface ChainEntry {
  /** Current chain key material (256 bits). Advances irreversibly per frame. */
  chainKeyRaw: ArrayBuffer
  /** Next frame counter we expect to derive for (starts at 1). */
  nextCtr: number
  /** Base salt for deterministic nonce computation (12 bytes, derived once from initial key). */
  baseSalt: Uint8Array
  /** Key generation identifier. */
  keyId: number
  /** Cached message keys for out-of-order frames (ctr → CryptoKey). */
  skippedKeys: Map<number, CryptoKey>
}

const GCM_TAG_LENGTH = 128 // bits
const HEADER_KID_LENGTH = 1
const HEADER_CTR_LENGTH = 4
const E2EE_HEADER_LENGTH = HEADER_KID_LENGTH + HEADER_CTR_LENGTH // 5 bytes
const TRAILER_LENGTH = 1

/** Maximum frames we'll skip forward (DoS protection). */
const MAX_SKIP = 256

const senderChains = new Map<string, ChainEntry[]>()
const frameCounters = new Map<string, number>()

// ── Per-participant mutex ─────────────────────────────────────────────
// Audio and video TransformStreams share the same chain entry per participant.
// Without serialization, their async transform() callbacks interleave at
// await points, causing both to read the same chain key before either advances
// it — corrupting the KDF chain permanently.

const chainLocks = new Map<string, Promise<void>>()

function withChainLock<T>(participantId: string, fn: () => Promise<T>): Promise<T> {
  const prev = chainLocks.get(participantId) ?? Promise.resolve()
  const next = prev.then(fn, fn)
  chainLocks.set(participantId, next.then(() => {}, () => {}))
  return next
}

// ── KDF Chain Derivation ────────────────────────────────────────────────

const HKDF_SALT = new Uint8Array(16) // fixed empty salt (IKM is already random)
const ENC_MSG = new TextEncoder().encode('msg')
const ENC_CHAIN = new TextEncoder().encode('chain')

/**
 * Initialise a chain entry from a raw sender key.
 * Derives the initial chain key (256 bits) and base salt (96 bits) via HKDF.
 */
async function initChainEntry(rawKey: ArrayBuffer, keyId: number): Promise<ChainEntry> {
  const keyMaterial = await crypto.subtle.importKey('raw', rawKey, 'HKDF', false, ['deriveBits'])
  const encoder = new TextEncoder()

  const chainKeyBits = await crypto.subtle.deriveBits(
    { name: 'HKDF', hash: 'SHA-256', salt: HKDF_SALT, info: encoder.encode(`seameet-chain-${keyId}`) },
    keyMaterial,
    256,
  )

  const saltBits = await crypto.subtle.deriveBits(
    { name: 'HKDF', hash: 'SHA-256', salt: HKDF_SALT, info: encoder.encode(`seameet-salt-${keyId}`) },
    keyMaterial,
    96, // 12 bytes
  )

  return {
    chainKeyRaw: chainKeyBits,
    nextCtr: 1,
    baseSalt: new Uint8Array(saltBits),
    keyId,
    skippedKeys: new Map(),
  }
}

/**
 * Derive a message key from the current chain key, then advance the chain.
 * This is the core KDF chain step:
 *   MK = HKDF(CK, "msg")     — used once for one frame, then discarded
 *   CK' = HKDF(CK, "chain")  — irreversible advancement
 */
async function stepChain(chainKeyRaw: ArrayBuffer): Promise<{ messageKey: CryptoKey; nextChainKeyRaw: ArrayBuffer }> {
  const keyMaterial = await crypto.subtle.importKey('raw', chainKeyRaw, 'HKDF', false, ['deriveBits', 'deriveKey'])

  const messageKey = await crypto.subtle.deriveKey(
    { name: 'HKDF', hash: 'SHA-256', salt: HKDF_SALT, info: ENC_MSG },
    keyMaterial,
    { name: 'AES-GCM', length: 256 },
    false,
    ['encrypt', 'decrypt'],
  )

  const nextChainBits = await crypto.subtle.deriveBits(
    { name: 'HKDF', hash: 'SHA-256', salt: HKDF_SALT, info: ENC_CHAIN },
    keyMaterial,
    256,
  )

  return { messageKey, nextChainKeyRaw: nextChainBits }
}

/**
 * Get the encryption key for the next frame (sender side).
 * Advances the chain by exactly one step.
 */
async function getEncryptionKey(entry: ChainEntry): Promise<CryptoKey> {
  const { messageKey, nextChainKeyRaw } = await stepChain(entry.chainKeyRaw)
  entry.chainKeyRaw = nextChainKeyRaw
  entry.nextCtr++
  return messageKey
}

/**
 * Get the decryption key for a specific frame counter (receiver side).
 * May advance the chain forward, caching intermediate message keys for
 * out-of-order frames within the window.
 */
async function getDecryptionKey(entry: ChainEntry, targetCtr: number): Promise<CryptoKey | null> {
  // 1. Check cached (skipped) keys
  const cached = entry.skippedKeys.get(targetCtr)
  if (cached) {
    entry.skippedKeys.delete(targetCtr)
    return cached
  }

  // 2. Frame is behind our chain — too old, key was already used/discarded
  if (targetCtr < entry.nextCtr) {
    return null
  }

  // 3. Frame is too far ahead — DoS protection
  const skip = targetCtr - entry.nextCtr
  if (skip > MAX_SKIP) {
    return null
  }

  // 4. Advance chain, caching intermediate message keys
  while (entry.nextCtr < targetCtr) {
    const { messageKey, nextChainKeyRaw } = await stepChain(entry.chainKeyRaw)
    entry.skippedKeys.set(entry.nextCtr, messageKey)
    entry.chainKeyRaw = nextChainKeyRaw
    entry.nextCtr++
  }

  // 5. Derive the target message key
  const { messageKey, nextChainKeyRaw } = await stepChain(entry.chainKeyRaw)
  entry.chainKeyRaw = nextChainKeyRaw
  entry.nextCtr++

  // 6. Evict oldest cached keys if over limit
  if (entry.skippedKeys.size > MAX_SKIP) {
    const sorted = [...entry.skippedKeys.keys()].sort((a, b) => a - b)
    for (const pos of sorted.slice(0, sorted.length - MAX_SKIP)) {
      entry.skippedKeys.delete(pos)
    }
  }

  return messageKey
}

// ── Deterministic Nonce ─────────────────────────────────────────────────

function computeNonce(salt: Uint8Array, ctr: number): Uint8Array {
  const nonce = new Uint8Array(12)
  nonce.set(salt)
  // XOR counter into the last 4 bytes of the salt
  const view = new DataView(nonce.buffer)
  view.setUint32(8, view.getUint32(8) ^ ctr)
  return nonce
}

// ── Replay Protection ───────────────────────────────────────────────────

interface ReplayWindow {
  highestCtr: number
  bitmap: bigint // sliding window behind highestCtr
}

const replayWindows = new Map<string, ReplayWindow>()

const REPLAY_WINDOW_SIZE = 128

function checkReplay(senderId: string, keyId: number, ctr: number): boolean {
  const windowKey = `${senderId}:${keyId}`
  const window = replayWindows.get(windowKey)

  if (!window) {
    replayWindows.set(windowKey, { highestCtr: ctr, bitmap: 1n })
    return true
  }

  if (ctr > window.highestCtr) {
    const shift = ctr - window.highestCtr
    if (shift < REPLAY_WINDOW_SIZE) {
      window.bitmap = (window.bitmap << BigInt(shift)) | 1n
    } else {
      window.bitmap = 1n
    }
    window.highestCtr = ctr
    return true
  }

  const diff = window.highestCtr - ctr
  if (diff >= REPLAY_WINDOW_SIZE) return false

  const bit = 1n << BigInt(diff)
  if (window.bitmap & bit) return false

  window.bitmap |= bit
  return true
}

// ── Codec Header Detection ──────────────────────────────────────────────

function getVP8UnencryptedBytes(data: ArrayBuffer): number {
  if (data.byteLength < 3) return 0
  const view = new DataView(data)
  const byte0 = view.getUint8(0)
  const isKeyframe = (byte0 & 0x01) === 0
  return isKeyframe ? 10 : 3
}

function getVP9UnencryptedBytes(data: ArrayBuffer): number {
  if (data.byteLength < 1) return 0
  const view = new DataView(data)
  const byte0 = view.getUint8(0)
  const profileLowBit = (byte0 >> 1) & 1
  if (profileLowBit === 0) return 1
  const showExistingFrame = (byte0 >> 3) & 1
  return showExistingFrame ? 1 : 3
}

/** Returns the number of unencrypted header bytes to preserve for SFU keyframe detection. */
function getUnencryptedBytes(metadata: RTCEncodedFrameMetadata, data: ArrayBuffer, isAudio: boolean): number {
  if (isAudio) return 0
  const pt = metadata.payloadType ?? 0
  if (pt >= 100 && pt < 110) return getVP9UnencryptedBytes(data)
  return getVP8UnencryptedBytes(data)
}

// ── AAD Construction ────────────────────────────────────────────────────

/** Build AAD = sender_id (UTF-8) + codec_header + e2ee_header */
function buildAAD(senderId: string, codecHeader: Uint8Array, e2eeHeader: Uint8Array): Uint8Array {
  const senderIdBytes = new TextEncoder().encode(senderId)
  const aad = new Uint8Array(senderIdBytes.length + codecHeader.length + e2eeHeader.length)
  let offset = 0
  aad.set(senderIdBytes, offset); offset += senderIdBytes.length
  aad.set(codecHeader, offset); offset += codecHeader.length
  aad.set(e2eeHeader, offset)
  return aad
}

// ── Encryption ──────────────────────────────────────────────────────────

async function encryptFrame(
  frame: RTCEncodedFrame,
  participantId: string,
  isAudio: boolean,
  controller: TransformStreamDefaultController<RTCEncodedFrame>,
) {
  const entries = senderChains.get(participantId)
  if (!entries || entries.length === 0) {
    controller.enqueue(frame)
    return
  }

  return withChainLock(participantId, async () => {
    const entry = entries[entries.length - 1]
    const data = frame.data
    const metadata = frame.getMetadata()
    const unencryptedBytes = getUnencryptedBytes(metadata, data, isAudio)

    const codecHeader = new Uint8Array(data, 0, unencryptedBytes)
    const payload = new Uint8Array(data, unencryptedBytes)

    // Increment frame counter (per participant + keyId)
    const ctrKey = `${participantId}:${entry.keyId}`
    const ctr = (frameCounters.get(ctrKey) ?? 0) + 1
    frameCounters.set(ctrKey, ctr)

    // Derive per-frame message key via KDF chain
    const messageKey = await getEncryptionKey(entry)

    // Deterministic nonce
    const iv = computeNonce(entry.baseSalt, ctr)

    // Build E2EE header: [KID 1 byte] [CTR 4 bytes big-endian]
    const e2eeHeader = new Uint8Array(E2EE_HEADER_LENGTH)
    e2eeHeader[0] = entry.keyId & 0xff
    new DataView(e2eeHeader.buffer).setUint32(HEADER_KID_LENGTH, ctr)

    // AAD = sender_id + codec header + e2ee header
    const aad = buildAAD(participantId, codecHeader, e2eeHeader)

    try {
      const ciphertext = await crypto.subtle.encrypt(
        { name: 'AES-GCM', iv, additionalData: aad, tagLength: GCM_TAG_LENGTH },
        messageKey,
        payload,
      )

      const ciphertextBytes = new Uint8Array(ciphertext)
      const outputLength = unencryptedBytes + E2EE_HEADER_LENGTH + ciphertextBytes.byteLength + TRAILER_LENGTH
      const output = new Uint8Array(outputLength)

      let offset = 0
      output.set(codecHeader, offset); offset += unencryptedBytes
      output.set(e2eeHeader, offset); offset += E2EE_HEADER_LENGTH
      output.set(ciphertextBytes, offset); offset += ciphertextBytes.byteLength
      // Trailer: lower 7 bits = unencrypted codec header size
      output[offset] = unencryptedBytes & 0x7f

      frame.data = output.buffer
      controller.enqueue(frame)
    } catch (e) {
      console.error('[E2EE Worker] encrypt error:', e)
      controller.enqueue(frame)
    }
  })
}

// ── Decryption ──────────────────────────────────────────────────────────

async function decryptFrame(
  frame: RTCEncodedFrame,
  senderId: string,
  isAudio: boolean,
  controller: TransformStreamDefaultController<RTCEncodedFrame>,
) {
  const entries = senderChains.get(senderId)
  if (!entries || entries.length === 0) {
    // No key yet for this sender — drop the frame rather than passing
    // encrypted data to the decoder (which would corrupt its state and
    // cause a permanent freeze even after the key arrives).
    return
  }

  const data = new Uint8Array(frame.data)
  const minSize = TRAILER_LENGTH + E2EE_HEADER_LENGTH + 16 // header + at least GCM tag
  if (data.byteLength < minSize) {
    // Frame too small to be E2EE-encrypted — drop it (not pass through,
    // which would feed encrypted/garbled data to the decoder).
    return
  }

  return withChainLock(senderId, async () => {
    // The entire decrypt body is wrapped in try/catch: if ANY exception
    // propagates (e.g. RangeError from a malformed/unencrypted frame whose
    // trailer yields an out-of-bounds DataView offset), the frame is silently
    // dropped instead of killing the TransformStream permanently.
    try {
      const trailer = data[data.byteLength - 1]
      const unencryptedBytes = trailer & 0x7f

      // Bounds check: unencryptedBytes must leave room for e2ee header + ciphertext + trailer
      if (unencryptedBytes + E2EE_HEADER_LENGTH + TRAILER_LENGTH > data.byteLength) {
        return // malformed frame — drop
      }

      const codecHeader = data.slice(0, unencryptedBytes)

      // Read KID and CTR from e2ee header
      const hdrStart = unencryptedBytes
      const kid = data[hdrStart]
      const ctr = new DataView(data.buffer, data.byteOffset + hdrStart + HEADER_KID_LENGTH, HEADER_CTR_LENGTH).getUint32(0)
      const e2eeHeader = data.slice(hdrStart, hdrStart + E2EE_HEADER_LENGTH)

      const ciphertext = data.slice(hdrStart + E2EE_HEADER_LENGTH, data.byteLength - TRAILER_LENGTH)

      // AAD = sender_id + codec header + e2ee header
      const aad = buildAAD(senderId, codecHeader, e2eeHeader)

      // Find matching chain entry by KID
      const matchingEntry = entries.find(e => (e.keyId & 0xff) === kid)
      if (matchingEntry) {
        if (!checkReplay(senderId, kid, ctr)) {
          return
        }

        // Derive per-frame message key via KDF chain
        const messageKey = await getDecryptionKey(matchingEntry, ctr)
        if (!messageKey) {
          return
        }

        const iv = computeNonce(matchingEntry.baseSalt, ctr)
        try {
          const plaintext = await crypto.subtle.decrypt(
            { name: 'AES-GCM', iv, additionalData: aad, tagLength: GCM_TAG_LENGTH },
            messageKey,
            ciphertext,
          )

          const output = new Uint8Array(unencryptedBytes + plaintext.byteLength)
          output.set(codecHeader, 0)
          output.set(new Uint8Array(plaintext), unencryptedBytes)
          frame.data = output.buffer
          controller.enqueue(frame)
          return
        } catch {
          // KID matched but decryption failed — frame corrupted or key mismatch during rotation
        }
      }

      // Fallback: try other chain entries (handles edge cases during key rotation)
      for (let i = entries.length - 1; i >= 0; i--) {
        const entry = entries[i]
        if (entry === matchingEntry) continue

        if (ctr < entry.nextCtr) {
          const cached = entry.skippedKeys.get(ctr)
          if (cached) {
            const iv = computeNonce(entry.baseSalt, ctr)
            try {
              const plaintext = await crypto.subtle.decrypt(
                { name: 'AES-GCM', iv, additionalData: aad, tagLength: GCM_TAG_LENGTH },
                cached,
                ciphertext,
              )
              entry.skippedKeys.delete(ctr)
              checkReplay(senderId, entry.keyId & 0xff, ctr)
              const output = new Uint8Array(unencryptedBytes + plaintext.byteLength)
              output.set(codecHeader, 0)
              output.set(new Uint8Array(plaintext), unencryptedBytes)
              frame.data = output.buffer
              controller.enqueue(frame)
              return
            } catch {
              // Try next entry
            }
          }
        }
      }
    } catch (e) {
      console.warn(`[E2EE Worker] decrypt error for sender ${senderId.slice(0, 8)}:`, e)
    }
  })
}

// ── Transform Setup ─────────────────────────────────────────────────────

function setupTransform(options: TransformOptions, readable: ReadableStream<RTCEncodedFrame>, writable: WritableStream<RTCEncodedFrame>) {
  const { operation, participantId, senderId } = options

  let detectedAudio: boolean | null = null

  const transform = new TransformStream<RTCEncodedFrame, RTCEncodedFrame>({
    transform(frame, controller) {
      if (detectedAudio === null) {
        const meta = frame.getMetadata()
        const pt = meta.payloadType ?? 0
        detectedAudio = pt === 111 || pt === 109 || pt === 110
      }

      if (operation === 'encrypt') {
        return encryptFrame(frame, participantId!, detectedAudio, controller)
      } else {
        return decryptFrame(frame, senderId!, detectedAudio, controller)
      }
    },
  })

  readable.pipeThrough(transform).pipeTo(writable)
}

// ── Message Handling ────────────────────────────────────────────────────

interface SetKeyMessage {
  type: 'setKey'
  participantId: string
  keyId: number
  rawKey: ArrayBuffer
}

interface RemoveKeysMessage {
  type: 'removeKeys'
  participantId: string
}

interface PurgeOldKeysMessage {
  type: 'purgeOldKeys'
  participantId: string
  keepKeyId: number
}

type WorkerMessage = SetKeyMessage | RemoveKeysMessage | PurgeOldKeysMessage

self.addEventListener('message', async (event: MessageEvent<WorkerMessage>) => {
  const msg = event.data

  if (msg.type === 'setKey') {
    const entry = await initChainEntry(msg.rawKey, msg.keyId)
    const entries = senderChains.get(msg.participantId) ?? []
    entries.push(entry)
    senderChains.set(msg.participantId, entries)
    console.log(`[E2EE Worker] setKey for ${msg.participantId.slice(0, 8)}, keyId=${msg.keyId} (KDF-chain, AES-256)`)
  }

  if (msg.type === 'removeKeys') {
    senderChains.delete(msg.participantId)
    // Clean up frame counters and replay windows for this participant
    for (const key of frameCounters.keys()) {
      if (key.startsWith(msg.participantId + ':')) frameCounters.delete(key)
    }
    for (const key of replayWindows.keys()) {
      if (key.startsWith(msg.participantId + ':')) replayWindows.delete(key)
    }
    console.log(`[E2EE Worker] removeKeys for ${msg.participantId.slice(0, 8)}`)
  }

  if (msg.type === 'purgeOldKeys') {
    const entries = senderChains.get(msg.participantId)
    if (entries) {
      const filtered = entries.filter(e => e.keyId >= msg.keepKeyId)
      senderChains.set(msg.participantId, filtered)
    }
  }
})

// ── RTCRtpScriptTransform entrypoint ────────────────────────────────────

// @ts-expect-error - rtctransform is a non-standard event for RTCRtpScriptTransform
self.addEventListener('rtctransform', (event: RTCTransformEvent) => {
  const { readable, writable, options } = event.transformer
  setupTransform(options, readable, writable)
})

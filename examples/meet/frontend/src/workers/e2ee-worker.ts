/**
 * E2EE Web Worker — AES-128-GCM encryption/decryption for encoded media frames.
 *
 * Frame format (v2 — SFrame-inspired):
 *   [codec header (N bytes)] [KID 1 byte] [CTR 4 bytes big-endian] [ciphertext + GCM tag 16 bytes] [trailer 1 byte = N]
 *
 * Key improvements over v1:
 *   - Deterministic nonce: salt XOR counter (no explicit IV in frame — saves 12 bytes/video frame)
 *   - HKDF key derivation: (key, salt) derived from base_key via HKDF-SHA256
 *   - KID in header: receiver selects key directly without brute-force
 *   - Replay protection: per-sender sliding window rejects duplicate/old frames
 *
 * AAD (additional authenticated data) = codec header + e2ee header (KID + CTR).
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

// ── Key Management ─────────────────────────────────────────────────────

interface KeyEntry {
  key: CryptoKey
  salt: Uint8Array // 12 bytes, derived via HKDF
  keyId: number
}

const senderKeys = new Map<string, KeyEntry[]>()
const GCM_TAG_LENGTH = 128 // bits
const HEADER_KID_LENGTH = 1
const HEADER_CTR_LENGTH = 4
const E2EE_HEADER_LENGTH = HEADER_KID_LENGTH + HEADER_CTR_LENGTH // 5 bytes
const TRAILER_LENGTH = 1

// Per-participant frame counters (for encryption)
const frameCounters = new Map<string, number>()

// ── HKDF Key Derivation ───────────────────────────────────────────────

async function deriveKeyAndSalt(rawKey: ArrayBuffer, keyId: number): Promise<{ key: CryptoKey; salt: Uint8Array }> {
  const keyMaterial = await crypto.subtle.importKey('raw', rawKey, 'HKDF', false, ['deriveBits', 'deriveKey'])
  const hkdfSalt = new Uint8Array(16) // fixed empty salt (ikm is already random)
  const encoder = new TextEncoder()

  const key = await crypto.subtle.deriveKey(
    { name: 'HKDF', hash: 'SHA-256', salt: hkdfSalt, info: encoder.encode(`seameet-e2ee-key-${keyId}`) },
    keyMaterial,
    { name: 'AES-GCM', length: 128 },
    false,
    ['encrypt', 'decrypt'],
  )

  const saltBits = await crypto.subtle.deriveBits(
    { name: 'HKDF', hash: 'SHA-256', salt: hkdfSalt, info: encoder.encode(`seameet-e2ee-salt-${keyId}`) },
    keyMaterial,
    96, // 12 bytes
  )

  return { key, salt: new Uint8Array(saltBits) }
}

// ── Deterministic Nonce ───────────────────────────────────────────────

function computeNonce(salt: Uint8Array, ctr: number): Uint8Array {
  const nonce = new Uint8Array(12)
  nonce.set(salt)
  // XOR counter into the last 4 bytes of the salt
  const view = new DataView(nonce.buffer)
  view.setUint32(8, view.getUint32(8) ^ ctr)
  return nonce
}

// ── Replay Protection ─────────────────────────────────────────────────

interface ReplayWindow {
  highestCtr: number
  bitmap: bigint // sliding window of 64 frames behind highestCtr
}

const replayWindows = new Map<string, ReplayWindow>()

const REPLAY_WINDOW_SIZE = 64

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

// ── Codec Header Detection ─────────────────────────────────────────────

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

// ── Encryption ─────────────────────────────────────────────────────────

async function encryptFrame(
  frame: RTCEncodedFrame,
  participantId: string,
  isAudio: boolean,
  controller: TransformStreamDefaultController<RTCEncodedFrame>,
) {
  const entries = senderKeys.get(participantId)
  if (!entries || entries.length === 0) {
    controller.enqueue(frame)
    return
  }

  const { key, salt, keyId } = entries[entries.length - 1]
  const data = frame.data
  const metadata = frame.getMetadata()
  const unencryptedBytes = getUnencryptedBytes(metadata, data, isAudio)

  const codecHeader = new Uint8Array(data, 0, unencryptedBytes)
  const payload = new Uint8Array(data, unencryptedBytes)

  // Increment frame counter (per participant + keyId)
  const ctrKey = `${participantId}:${keyId}`
  const ctr = (frameCounters.get(ctrKey) ?? 0) + 1
  frameCounters.set(ctrKey, ctr)

  // Deterministic nonce
  const iv = computeNonce(salt, ctr)

  // Build E2EE header: [KID 1 byte] [CTR 4 bytes big-endian]
  const e2eeHeader = new Uint8Array(E2EE_HEADER_LENGTH)
  e2eeHeader[0] = keyId & 0xff
  new DataView(e2eeHeader.buffer).setUint32(HEADER_KID_LENGTH, ctr)

  // AAD = codec header + e2ee header
  const aad = new Uint8Array(unencryptedBytes + E2EE_HEADER_LENGTH)
  aad.set(codecHeader, 0)
  aad.set(e2eeHeader, unencryptedBytes)

  try {
    const ciphertext = await crypto.subtle.encrypt(
      { name: 'AES-GCM', iv, additionalData: aad, tagLength: GCM_TAG_LENGTH },
      key,
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
}

// ── Decryption ─────────────────────────────────────────────────────────

async function decryptFrame(
  frame: RTCEncodedFrame,
  senderId: string,
  isAudio: boolean,
  controller: TransformStreamDefaultController<RTCEncodedFrame>,
) {
  const entries = senderKeys.get(senderId)
  if (!entries || entries.length === 0) {
    controller.enqueue(frame)
    return
  }

  const data = new Uint8Array(frame.data)
  const minSize = TRAILER_LENGTH + E2EE_HEADER_LENGTH + 16 // header + at least GCM tag
  if (data.byteLength < minSize) {
    controller.enqueue(frame)
    return
  }

  const trailer = data[data.byteLength - 1]
  const unencryptedBytes = trailer & 0x7f

  const codecHeader = data.slice(0, unencryptedBytes)

  // Read KID and CTR from e2ee header
  const hdrStart = unencryptedBytes
  const kid = data[hdrStart]
  const ctr = new DataView(data.buffer, data.byteOffset + hdrStart + HEADER_KID_LENGTH, HEADER_CTR_LENGTH).getUint32(0)
  const e2eeHeader = data.slice(hdrStart, hdrStart + E2EE_HEADER_LENGTH)

  const ciphertext = data.slice(hdrStart + E2EE_HEADER_LENGTH, data.byteLength - TRAILER_LENGTH)

  // AAD = codec header + e2ee header
  const aad = new Uint8Array(unencryptedBytes + E2EE_HEADER_LENGTH)
  aad.set(codecHeader, 0)
  aad.set(e2eeHeader, unencryptedBytes)

  // Find matching key by KID (direct lookup, no brute-force)
  const matchingEntry = entries.find(e => (e.keyId & 0xff) === kid)
  if (matchingEntry) {
    if (!checkReplay(senderId, kid, ctr)) {
      console.warn(`[E2EE Worker] replay detected for sender ${senderId.slice(0, 8)}, ctr=${ctr}`)
      return
    }

    const iv = computeNonce(matchingEntry.salt, ctr)
    try {
      const plaintext = await crypto.subtle.decrypt(
        { name: 'AES-GCM', iv, additionalData: aad, tagLength: GCM_TAG_LENGTH },
        matchingEntry.key,
        ciphertext,
      )

      const output = new Uint8Array(unencryptedBytes + plaintext.byteLength)
      output.set(codecHeader, 0)
      output.set(new Uint8Array(plaintext), unencryptedBytes)
      frame.data = output.buffer
      controller.enqueue(frame)
      return
    } catch {
      // KID matched but decryption failed — fall through to try other keys
    }
  }

  // Fallback: try all keys (handles edge cases during key rotation)
  for (let i = entries.length - 1; i >= 0; i--) {
    const entry = entries[i]
    if (entry === matchingEntry) continue

    const iv = computeNonce(entry.salt, ctr)
    try {
      const plaintext = await crypto.subtle.decrypt(
        { name: 'AES-GCM', iv, additionalData: aad, tagLength: GCM_TAG_LENGTH },
        entry.key,
        ciphertext,
      )

      checkReplay(senderId, entry.keyId & 0xff, ctr)

      const output = new Uint8Array(unencryptedBytes + plaintext.byteLength)
      output.set(codecHeader, 0)
      output.set(new Uint8Array(plaintext), unencryptedBytes)
      frame.data = output.buffer
      controller.enqueue(frame)
      return
    } catch {
      // Try next key
    }
  }

  console.warn(`[E2EE Worker] decrypt failed for sender ${senderId.slice(0, 8)} — dropping frame`)
}

// ── Transform Setup ────────────────────────────────────────────────────

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
        encryptFrame(frame, participantId!, detectedAudio, controller)
      } else {
        decryptFrame(frame, senderId!, detectedAudio, controller)
      }
    },
  })

  readable.pipeThrough(transform).pipeTo(writable)
}

// ── Message Handling ───────────────────────────────────────────────────

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
    const { key, salt } = await deriveKeyAndSalt(msg.rawKey, msg.keyId)
    const entries = senderKeys.get(msg.participantId) ?? []
    entries.push({ key, salt, keyId: msg.keyId })
    senderKeys.set(msg.participantId, entries)
    console.log(`[E2EE Worker] setKey for ${msg.participantId.slice(0, 8)}, keyId=${msg.keyId} (HKDF-derived)`)
  }

  if (msg.type === 'removeKeys') {
    senderKeys.delete(msg.participantId)
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
    const entries = senderKeys.get(msg.participantId)
    if (entries) {
      const filtered = entries.filter(e => e.keyId >= msg.keepKeyId)
      senderKeys.set(msg.participantId, filtered)
    }
  }
})

// ── RTCRtpScriptTransform entrypoint ───────────────────────────────────

// @ts-expect-error - rtctransform is a non-standard event for RTCRtpScriptTransform
self.addEventListener('rtctransform', (event: RTCTransformEvent) => {
  const { readable, writable, options } = event.transformer
  setupTransform(options, readable, writable)
})

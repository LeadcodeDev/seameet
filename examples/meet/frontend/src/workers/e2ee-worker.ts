/**
 * E2EE Web Worker — AES-128-GCM encryption/decryption for encoded media frames.
 *
 * Frame format:
 *   [unencrypted codec header (N bytes)] [IV 12 bytes] [encrypted payload] [GCM tag 16 bytes] [trailer 1 byte = N]
 *
 * Audio (Opus) uses implicit IV derived from SSRC + sequence number → no explicit IV in frame.
 */

// Type declarations for RTCEncodedFrame APIs available inside a Worker scope
interface RTCEncodedFrame {
  readonly data: ArrayBuffer
  readonly timestamp: number
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
  keyId: number
}

const senderKeys = new Map<string, KeyEntry[]>()
const GCM_TAG_LENGTH = 128 // bits
const IV_LENGTH = 12 // bytes
const TRAILER_LENGTH = 1 // byte encoding the unencrypted header size

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

  // Detect codec from payloadType — VP8 is typically 96-99, VP9 100-103
  // In practice we try VP8 first since it's the most common fallback
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
    // No key yet — pass through unencrypted
    controller.enqueue(frame)
    return
  }

  const { key, keyId } = entries[entries.length - 1]
  const data = frame.data
  const metadata = frame.getMetadata()
  const unencryptedBytes = getUnencryptedBytes(metadata, data, isAudio)

  const header = new Uint8Array(data, 0, unencryptedBytes)
  const payload = new Uint8Array(data, unencryptedBytes)

  let iv: Uint8Array
  let ivInFrame: boolean

  if (isAudio && metadata.synchronizationSource !== undefined && metadata.sequenceNumber !== undefined) {
    // Implicit IV for audio: SSRC (4 bytes) + seq_no (4 bytes) + keyId (4 bytes)
    iv = new Uint8Array(IV_LENGTH)
    const dvIv = new DataView(iv.buffer)
    dvIv.setUint32(0, metadata.synchronizationSource)
    dvIv.setUint32(4, metadata.sequenceNumber)
    dvIv.setUint32(8, keyId)
    ivInFrame = false
  } else {
    // Explicit random IV for video
    iv = crypto.getRandomValues(new Uint8Array(IV_LENGTH))
    ivInFrame = true
  }

  try {
    const ciphertext = await crypto.subtle.encrypt(
      { name: 'AES-GCM', iv, additionalData: header, tagLength: GCM_TAG_LENGTH },
      key,
      payload,
    )

    const ciphertextBytes = new Uint8Array(ciphertext)
    const outputLength = unencryptedBytes + (ivInFrame ? IV_LENGTH : 0) + ciphertextBytes.byteLength + TRAILER_LENGTH
    const output = new Uint8Array(outputLength)

    let offset = 0
    output.set(header, offset); offset += unencryptedBytes
    if (ivInFrame) { output.set(iv, offset); offset += IV_LENGTH }
    output.set(ciphertextBytes, offset); offset += ciphertextBytes.byteLength
    // Trailer: high bit = ivInFrame flag, lower 7 bits = unencryptedBytes
    output[offset] = (ivInFrame ? 0x80 : 0x00) | (unencryptedBytes & 0x7f)

    frame.data = output.buffer
    controller.enqueue(frame)
  } catch (e) {
    console.error('[E2EE Worker] encrypt error:', e)
    // Pass through unencrypted on failure
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
    // No key — pass through (unencrypted stream)
    controller.enqueue(frame)
    return
  }

  const data = new Uint8Array(frame.data)
  if (data.byteLength < TRAILER_LENGTH + 16) {
    // Too small to be encrypted — pass through
    controller.enqueue(frame)
    return
  }

  const trailer = data[data.byteLength - 1]
  const ivInFrame = (trailer & 0x80) !== 0
  const unencryptedBytes = trailer & 0x7f
  const metadata = frame.getMetadata()

  const header = data.slice(0, unencryptedBytes)

  let iv: Uint8Array
  let ciphertextStart: number

  if (ivInFrame) {
    iv = data.slice(unencryptedBytes, unencryptedBytes + IV_LENGTH)
    ciphertextStart = unencryptedBytes + IV_LENGTH
  } else {
    // Implicit IV for audio
    iv = new Uint8Array(IV_LENGTH)
    const dvIv = new DataView(iv.buffer)
    dvIv.setUint32(0, metadata.synchronizationSource ?? 0)
    dvIv.setUint32(4, metadata.sequenceNumber ?? 0)
    // Try all key IDs for implicit IV
    ciphertextStart = unencryptedBytes
  }

  const ciphertext = data.slice(ciphertextStart, data.byteLength - TRAILER_LENGTH)

  // Try all keys (most recent first) to handle key rotation gracefully
  for (let i = entries.length - 1; i >= 0; i--) {
    const { key, keyId } = entries[i]

    let actualIv = iv
    if (!ivInFrame) {
      actualIv = new Uint8Array(IV_LENGTH)
      const dvIv = new DataView(actualIv.buffer)
      dvIv.setUint32(0, metadata.synchronizationSource ?? 0)
      dvIv.setUint32(4, metadata.sequenceNumber ?? 0)
      dvIv.setUint32(8, keyId)
    }

    try {
      const plaintext = await crypto.subtle.decrypt(
        { name: 'AES-GCM', iv: actualIv, additionalData: header, tagLength: GCM_TAG_LENGTH },
        key,
        ciphertext,
      )

      const output = new Uint8Array(unencryptedBytes + plaintext.byteLength)
      output.set(header, 0)
      output.set(new Uint8Array(plaintext), unencryptedBytes)
      frame.data = output.buffer
      controller.enqueue(frame)
      return
    } catch {
      // Try next key
    }
  }

  // All keys failed — drop frame silently (key not yet received or rotated)
  console.warn(`[E2EE Worker] decrypt failed for sender ${senderId.slice(0, 8)} — dropping frame`)
}

// ── Transform Setup ────────────────────────────────────────────────────

function setupTransform(options: TransformOptions, readable: ReadableStream<RTCEncodedFrame>, writable: WritableStream<RTCEncodedFrame>) {
  const { operation, participantId, senderId } = options

  // Detect audio by checking track kind from initial frames
  let detectedAudio: boolean | null = null

  const transform = new TransformStream<RTCEncodedFrame, RTCEncodedFrame>({
    transform(frame, controller) {
      // Heuristic: audio frames are typically very small (<200 bytes)
      // and video frames are larger. We check payloadType from metadata.
      if (detectedAudio === null) {
        const meta = frame.getMetadata()
        const pt = meta.payloadType ?? 0
        // Common audio payload types: 111 (opus)
        // Common video payload types: 96-110 (VP8/VP9/H264)
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
    const cryptoKey = await crypto.subtle.importKey(
      'raw',
      msg.rawKey,
      { name: 'AES-GCM', length: 128 },
      false,
      ['encrypt', 'decrypt'],
    )
    const entries = senderKeys.get(msg.participantId) ?? []
    entries.push({ key: cryptoKey, keyId: msg.keyId })
    senderKeys.set(msg.participantId, entries)
    console.log(`[E2EE Worker] setKey for ${msg.participantId.slice(0, 8)}, keyId=${msg.keyId}`)
  }

  if (msg.type === 'removeKeys') {
    senderKeys.delete(msg.participantId)
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

/**
 * Pure crypto functions extracted from e2ee-worker.ts for testability.
 * The worker imports from this module; tests import directly.
 */

// ── Types ───────────────────────────────────────────────────────────────

export interface ChainEntry {
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

export interface ReplayWindow {
  highestCtr: number
  bitmap: bigint
}

// ── Constants ───────────────────────────────────────────────────────────

export const GCM_TAG_LENGTH = 128 // bits
export const HEADER_KID_LENGTH = 1
export const HEADER_CTR_LENGTH = 4
export const E2EE_HEADER_LENGTH = HEADER_KID_LENGTH + HEADER_CTR_LENGTH // 5 bytes
export const TRAILER_LENGTH = 1
export const MAX_SKIP = 256
export const REPLAY_WINDOW_SIZE = 128

// ── KDF Chain Derivation ────────────────────────────────────────────────

const HKDF_SALT = new Uint8Array(16)
const ENC_MSG = new TextEncoder().encode('msg')
const ENC_CHAIN = new TextEncoder().encode('chain')

export async function initChainEntry(rawKey: ArrayBuffer, keyId: number): Promise<ChainEntry> {
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
    96,
  )

  return {
    chainKeyRaw: chainKeyBits,
    nextCtr: 1,
    baseSalt: new Uint8Array(saltBits),
    keyId,
    skippedKeys: new Map(),
  }
}

export async function stepChain(chainKeyRaw: ArrayBuffer): Promise<{ messageKey: CryptoKey; nextChainKeyRaw: ArrayBuffer }> {
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

export async function getEncryptionKey(entry: ChainEntry): Promise<CryptoKey> {
  const { messageKey, nextChainKeyRaw } = await stepChain(entry.chainKeyRaw)
  entry.chainKeyRaw = nextChainKeyRaw
  entry.nextCtr++
  return messageKey
}

export async function getDecryptionKey(entry: ChainEntry, targetCtr: number): Promise<CryptoKey | null> {
  const cached = entry.skippedKeys.get(targetCtr)
  if (cached) {
    entry.skippedKeys.delete(targetCtr)
    return cached
  }

  if (targetCtr < entry.nextCtr) {
    return null
  }

  const skip = targetCtr - entry.nextCtr
  if (skip > MAX_SKIP) {
    return null
  }

  while (entry.nextCtr < targetCtr) {
    const { messageKey, nextChainKeyRaw } = await stepChain(entry.chainKeyRaw)
    entry.skippedKeys.set(entry.nextCtr, messageKey)
    entry.chainKeyRaw = nextChainKeyRaw
    entry.nextCtr++
  }

  const { messageKey, nextChainKeyRaw } = await stepChain(entry.chainKeyRaw)
  entry.chainKeyRaw = nextChainKeyRaw
  entry.nextCtr++

  if (entry.skippedKeys.size > MAX_SKIP) {
    const sorted = [...entry.skippedKeys.keys()].sort((a, b) => a - b)
    for (const pos of sorted.slice(0, sorted.length - MAX_SKIP)) {
      entry.skippedKeys.delete(pos)
    }
  }

  return messageKey
}

// ── Deterministic Nonce ─────────────────────────────────────────────────

export function computeNonce(salt: Uint8Array, ctr: number): Uint8Array {
  const nonce = new Uint8Array(12)
  nonce.set(salt)
  const view = new DataView(nonce.buffer)
  view.setUint32(8, view.getUint32(8) ^ ctr)
  return nonce
}

// ── Replay Protection ───────────────────────────────────────────────────

export function checkReplay(
  replayWindows: Map<string, ReplayWindow>,
  senderId: string,
  keyId: number,
  ctr: number,
): boolean {
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

// ── AAD Construction ────────────────────────────────────────────────────

export function buildAAD(senderId: string, codecHeader: Uint8Array, e2eeHeader: Uint8Array): Uint8Array {
  const senderIdBytes = new TextEncoder().encode(senderId)
  const aad = new Uint8Array(senderIdBytes.length + codecHeader.length + e2eeHeader.length)
  let offset = 0
  aad.set(senderIdBytes, offset); offset += senderIdBytes.length
  aad.set(codecHeader, offset); offset += codecHeader.length
  aad.set(e2eeHeader, offset)
  return aad
}

// ── Codec Header Detection ──────────────────────────────────────────────

export function getVP8UnencryptedBytes(data: ArrayBuffer): number {
  if (data.byteLength < 3) return 0
  const view = new DataView(data)
  const byte0 = view.getUint8(0)
  const isKeyframe = (byte0 & 0x01) === 0
  return isKeyframe ? 10 : 3
}

export function getVP9UnencryptedBytes(data: ArrayBuffer): number {
  if (data.byteLength < 1) return 0
  const view = new DataView(data)
  const byte0 = view.getUint8(0)
  const profileLowBit = (byte0 >> 1) & 1
  if (profileLowBit === 0) return 1
  const showExistingFrame = (byte0 >> 3) & 1
  return showExistingFrame ? 1 : 3
}

export function getUnencryptedBytes(
  metadata: { payloadType?: number },
  data: ArrayBuffer,
  isAudio: boolean,
): number {
  if (isAudio) return 0
  const pt = metadata.payloadType ?? 0
  if (pt >= 100 && pt < 110) return getVP9UnencryptedBytes(data)
  return getVP8UnencryptedBytes(data)
}

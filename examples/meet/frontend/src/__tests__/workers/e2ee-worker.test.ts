import { describe, it, expect, beforeEach } from 'vitest'
import {
  initChainEntry,
  stepChain,
  getEncryptionKey,
  getDecryptionKey,
  computeNonce,
  buildAAD,
  checkReplay,
  getUnencryptedBytes,
  getVP8UnencryptedBytes,
  getVP9UnencryptedBytes,
  MAX_SKIP,
  GCM_TAG_LENGTH,
  E2EE_HEADER_LENGTH,
  TRAILER_LENGTH,
  REPLAY_WINDOW_SIZE,
  type ChainEntry,
  type ReplayWindow,
} from '@/workers/e2ee-crypto'

// ── Helpers ─────────────────────────────────────────────────────────────

async function generateRawKey(): Promise<ArrayBuffer> {
  const key = await crypto.subtle.generateKey({ name: 'AES-GCM', length: 256 }, true, ['encrypt', 'decrypt'])
  return crypto.subtle.exportKey('raw', key)
}

// ── initChainEntry ──────────────────────────────────────────────────────

describe('initChainEntry', () => {
  it('produces chain key and salt with correct sizes', async () => {
    const rawKey = await generateRawKey()
    const entry = await initChainEntry(rawKey, 0)

    expect(entry.chainKeyRaw.byteLength).toBe(32) // 256 bits
    expect(entry.baseSalt.byteLength).toBe(12) // 96 bits
    expect(entry.nextCtr).toBe(1)
    expect(entry.keyId).toBe(0)
    expect(entry.skippedKeys.size).toBe(0)
  })

  it('produces different chain keys for different keyIds', async () => {
    const rawKey = await generateRawKey()
    const entry0 = await initChainEntry(rawKey, 0)
    const entry1 = await initChainEntry(rawKey, 1)

    const ck0 = new Uint8Array(entry0.chainKeyRaw)
    const ck1 = new Uint8Array(entry1.chainKeyRaw)
    expect(ck0).not.toEqual(ck1)
  })

  it('is deterministic for same rawKey and keyId', async () => {
    const rawKey = await generateRawKey()
    const entry1 = await initChainEntry(rawKey, 42)
    const entry2 = await initChainEntry(rawKey, 42)

    expect(new Uint8Array(entry1.chainKeyRaw)).toEqual(new Uint8Array(entry2.chainKeyRaw))
    expect(entry1.baseSalt).toEqual(entry2.baseSalt)
  })
})

// ── stepChain ───────────────────────────────────────────────────────────

describe('stepChain', () => {
  it('advances irreversibly (CK_n != CK_n+1)', async () => {
    const rawKey = await generateRawKey()
    const entry = await initChainEntry(rawKey, 0)
    const ckBefore = new Uint8Array(entry.chainKeyRaw).slice()

    const { nextChainKeyRaw } = await stepChain(entry.chainKeyRaw)

    expect(new Uint8Array(nextChainKeyRaw)).not.toEqual(ckBefore)
  })

  it('derives a deterministic message key', async () => {
    const rawKey = await generateRawKey()
    // Create two entries from the same key — they should produce the same first step
    const entry1 = await initChainEntry(rawKey, 0)
    const entry2 = await initChainEntry(rawKey, 0)

    const step1 = await stepChain(entry1.chainKeyRaw)
    const step2 = await stepChain(entry2.chainKeyRaw)

    expect(new Uint8Array(step1.nextChainKeyRaw)).toEqual(new Uint8Array(step2.nextChainKeyRaw))
  })
})

// ── getEncryptionKey ────────────────────────────────────────────────────

describe('getEncryptionKey', () => {
  it('advances nextCtr and returns a CryptoKey', async () => {
    const rawKey = await generateRawKey()
    const entry = await initChainEntry(rawKey, 0)
    expect(entry.nextCtr).toBe(1)

    const key = await getEncryptionKey(entry)
    expect(key).toBeDefined()
    expect(entry.nextCtr).toBe(2)

    const key2 = await getEncryptionKey(entry)
    expect(key2).toBeDefined()
    expect(entry.nextCtr).toBe(3)
  })
})

// ── getDecryptionKey ────────────────────────────────────────────────────

describe('getDecryptionKey', () => {
  it('derives direct when ctr == nextCtr', async () => {
    const rawKey = await generateRawKey()
    const entry = await initChainEntry(rawKey, 0)
    // nextCtr starts at 1
    const key = await getDecryptionKey(entry, 1)
    expect(key).not.toBeNull()
    expect(entry.nextCtr).toBe(2)
  })

  it('caches intermediaries for out-of-order (ctr > nextCtr)', async () => {
    const rawKey = await generateRawKey()
    const entry = await initChainEntry(rawKey, 0)
    // Skip to ctr=5 (nextCtr=1, skip 1,2,3,4)
    const key = await getDecryptionKey(entry, 5)
    expect(key).not.toBeNull()
    expect(entry.nextCtr).toBe(6)
    // Intermediate keys 1-4 should be cached
    expect(entry.skippedKeys.size).toBe(4)
    expect(entry.skippedKeys.has(1)).toBe(true)
    expect(entry.skippedKeys.has(4)).toBe(true)
  })

  it('returns null for ctr < nextCtr (too old)', async () => {
    const rawKey = await generateRawKey()
    const entry = await initChainEntry(rawKey, 0)
    // Advance chain sequentially by consuming keys 1, 2, 3
    await getDecryptionKey(entry, 1)
    await getDecryptionKey(entry, 2)
    await getDecryptionKey(entry, 3)
    // nextCtr is now 4, so ctr=1 is behind and no cached key exists
    const key = await getDecryptionKey(entry, 1)
    expect(key).toBeNull()
  })

  it('returns null when ctr - nextCtr > MAX_SKIP (DoS protection)', async () => {
    const rawKey = await generateRawKey()
    const entry = await initChainEntry(rawKey, 0)
    const key = await getDecryptionKey(entry, MAX_SKIP + 2)
    expect(key).toBeNull()
    // Chain should not have advanced
    expect(entry.nextCtr).toBe(1)
  })

  it('consumes cached key and removes it', async () => {
    const rawKey = await generateRawKey()
    const entry = await initChainEntry(rawKey, 0)
    // Skip to 3, caching 1 and 2
    await getDecryptionKey(entry, 3)
    expect(entry.skippedKeys.has(1)).toBe(true)

    const key = await getDecryptionKey(entry, 1)
    expect(key).not.toBeNull()
    expect(entry.skippedKeys.has(1)).toBe(false)
  })
})

// ── computeNonce ────────────────────────────────────────────────────────

describe('computeNonce', () => {
  it('produces deterministic 12-byte nonces', () => {
    const salt = new Uint8Array(12)
    salt.fill(0xab)
    const n1 = computeNonce(salt, 42)
    const n2 = computeNonce(salt, 42)
    expect(n1).toEqual(n2)
    expect(n1.byteLength).toBe(12)
  })

  it('XORs counter into last 4 bytes', () => {
    const salt = new Uint8Array(12) // all zeros
    const nonce = computeNonce(salt, 1)
    // Last 4 bytes = 0x00000000 XOR 0x00000001
    const view = new DataView(nonce.buffer)
    expect(view.getUint32(8)).toBe(1)
  })

  it('produces different nonces for different counters', () => {
    const salt = new Uint8Array(12)
    const n1 = computeNonce(salt, 1)
    const n2 = computeNonce(salt, 2)
    expect(n1).not.toEqual(n2)
  })
})

// ── buildAAD ────────────────────────────────────────────────────────────

describe('buildAAD', () => {
  it('concatenates sender_id + codec_header + e2ee_header', () => {
    const codec = new Uint8Array([0x90, 0x91])
    const e2ee = new Uint8Array([0x00, 0x00, 0x00, 0x00, 0x01])
    const aad = buildAAD('alice', codec, e2ee)

    const encoder = new TextEncoder()
    const aliceBytes = encoder.encode('alice')
    expect(aad.byteLength).toBe(aliceBytes.length + codec.length + e2ee.length)
    // Verify sender_id prefix
    expect(Array.from(aad.slice(0, aliceBytes.length))).toEqual(Array.from(aliceBytes))
    // Verify codec header
    expect(Array.from(aad.slice(aliceBytes.length, aliceBytes.length + 2))).toEqual(Array.from(codec))
    // Verify e2ee header
    expect(Array.from(aad.slice(aliceBytes.length + 2))).toEqual(Array.from(e2ee))
  })
})

// ── checkReplay ─────────────────────────────────────────────────────────

describe('checkReplay', () => {
  let windows: Map<string, ReplayWindow>

  beforeEach(() => {
    windows = new Map()
  })

  it('accepts the first frame', () => {
    expect(checkReplay(windows, 'alice', 0, 1)).toBe(true)
  })

  it('rejects duplicate frame', () => {
    checkReplay(windows, 'alice', 0, 1)
    expect(checkReplay(windows, 'alice', 0, 1)).toBe(false)
  })

  it('accepts frame within sliding window', () => {
    checkReplay(windows, 'alice', 0, 100)
    // Frame 50 is within 128-frame window behind 100
    expect(checkReplay(windows, 'alice', 0, 50)).toBe(true)
  })

  it('rejects frame outside sliding window', () => {
    checkReplay(windows, 'alice', 0, 200)
    // Frame 1 is 199 behind 200, window is 128
    expect(checkReplay(windows, 'alice', 0, 1)).toBe(false)
  })

  it('accepts newer frames and advances window', () => {
    checkReplay(windows, 'alice', 0, 1)
    expect(checkReplay(windows, 'alice', 0, 2)).toBe(true)
    expect(checkReplay(windows, 'alice', 0, 3)).toBe(true)
  })

  it('handles large jump (shift >= REPLAY_WINDOW_SIZE)', () => {
    checkReplay(windows, 'alice', 0, 1)
    // Jump way ahead — resets bitmap
    expect(checkReplay(windows, 'alice', 0, 1000)).toBe(true)
    // Old frame is now outside window
    expect(checkReplay(windows, 'alice', 0, 1)).toBe(false)
  })
})

// ── Codec Header Detection ──────────────────────────────────────────────

describe('getUnencryptedBytes', () => {
  it('returns 0 for audio', () => {
    const data = new ArrayBuffer(100)
    expect(getUnencryptedBytes({ payloadType: 111 }, data, true)).toBe(0)
  })

  it('returns VP8 bytes for video with low payload type', () => {
    // VP8 non-keyframe (bit 0 = 1)
    const data = new ArrayBuffer(10)
    new Uint8Array(data)[0] = 0x01
    expect(getVP8UnencryptedBytes(data)).toBe(3)
  })

  it('returns VP8 keyframe bytes', () => {
    // VP8 keyframe (bit 0 = 0)
    const data = new ArrayBuffer(20)
    new Uint8Array(data)[0] = 0x00
    expect(getVP8UnencryptedBytes(data)).toBe(10)
  })

  it('returns 0 for too-small VP8 data', () => {
    expect(getVP8UnencryptedBytes(new ArrayBuffer(2))).toBe(0)
  })

  it('returns VP9 bytes for high payload type', () => {
    const data = new ArrayBuffer(10)
    new Uint8Array(data)[0] = 0x00 // profileLowBit=0
    expect(getUnencryptedBytes({ payloadType: 105 }, data, false)).toBe(1)
  })
})

// ── Encrypt/Decrypt Round Trip ──────────────────────────────────────────

describe('encrypt → decrypt round-trip', () => {
  it('recovers plaintext identically', async () => {
    const rawKey = await generateRawKey()
    const senderEntry = await initChainEntry(rawKey, 0)
    const receiverEntry = await initChainEntry(rawKey, 0)

    const plaintext = new TextEncoder().encode('hello e2ee')

    // Encrypt
    const encKey = await getEncryptionKey(senderEntry)
    const ctr = 1
    const iv = computeNonce(senderEntry.baseSalt, ctr)
    const aad = buildAAD('alice', new Uint8Array(0), new Uint8Array([0, 0, 0, 0, ctr]))

    const ciphertext = await crypto.subtle.encrypt(
      { name: 'AES-GCM', iv, additionalData: aad, tagLength: GCM_TAG_LENGTH },
      encKey,
      plaintext,
    )

    // Decrypt
    const decKey = await getDecryptionKey(receiverEntry, ctr)
    expect(decKey).not.toBeNull()
    const iv2 = computeNonce(receiverEntry.baseSalt, ctr)
    const decrypted = await crypto.subtle.decrypt(
      { name: 'AES-GCM', iv: iv2, additionalData: aad, tagLength: GCM_TAG_LENGTH },
      decKey!,
      ciphertext,
    )

    expect(Array.from(new Uint8Array(decrypted))).toEqual(Array.from(plaintext))
  })
})

// ── Constants ───────────────────────────────────────────────────────────

describe('constants', () => {
  it('has correct values', () => {
    expect(GCM_TAG_LENGTH).toBe(128)
    expect(E2EE_HEADER_LENGTH).toBe(5)
    expect(TRAILER_LENGTH).toBe(1)
    expect(MAX_SKIP).toBe(256)
    expect(REPLAY_WINDOW_SIZE).toBe(128)
  })
})

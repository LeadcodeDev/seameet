import { describe, it, expect, beforeEach, vi, afterEach } from 'vitest'
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
  MAX_INITIAL_SKIP,
  GCM_TAG_LENGTH,
  HEADER_KID_LENGTH,
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

  it('caches intermediaries for out-of-order (ctr > nextCtr) after catch-up', async () => {
    const rawKey = await generateRawKey()
    const entry = await initChainEntry(rawKey, 0)
    // Establish baseline so the catch-up branch is no longer used.
    await getDecryptionKey(entry, 1)
    // Now skip to ctr=5 (nextCtr=2, skip 2,3,4)
    const key = await getDecryptionKey(entry, 5)
    expect(key).not.toBeNull()
    expect(entry.nextCtr).toBe(6)
    // Intermediate keys 2-4 should be cached
    expect(entry.skippedKeys.size).toBe(3)
    expect(entry.skippedKeys.has(2)).toBe(true)
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

  it('first decryption catches up from large skip without caching', async () => {
    // Late-joiner scenario: encryptor has been running for ~15s @ 30fps,
    // so the first frame we decrypt has ctr ≈ 450 — well above MAX_SKIP.
    const rawKey = await generateRawKey()
    const entry = await initChainEntry(rawKey, 0)
    const target = MAX_SKIP + 200 // 456: legitimate "we just joined" gap
    const key = await getDecryptionKey(entry, target)
    expect(key).not.toBeNull()
    expect(entry.nextCtr).toBe(target + 1)
    // No caching during catch-up — those frames are already gone on the wire.
    expect(entry.skippedKeys.size).toBe(0)
    expect(entry.caughtUp).toBe(true)
  })

  it('catch-up still bounded by MAX_INITIAL_SKIP', async () => {
    const rawKey = await generateRawKey()
    const entry = await initChainEntry(rawKey, 0)
    const key = await getDecryptionKey(entry, MAX_INITIAL_SKIP + 2)
    expect(key).toBeNull()
    expect(entry.nextCtr).toBe(1)
    expect(entry.caughtUp).toBe(false)
  })

  it('returns null when ctr - nextCtr > MAX_SKIP after catch-up (DoS protection)', async () => {
    const rawKey = await generateRawKey()
    const entry = await initChainEntry(rawKey, 0)
    // Catch-up first so the strict MAX_SKIP rule applies thereafter.
    await getDecryptionKey(entry, 1)
    const key = await getDecryptionKey(entry, entry.nextCtr + MAX_SKIP + 2)
    expect(key).toBeNull()
  })

  it('consumes cached key and removes it', async () => {
    const rawKey = await generateRawKey()
    const entry = await initChainEntry(rawKey, 0)
    // Catch-up to seed nextCtr without caching.
    await getDecryptionKey(entry, 1)
    // Skip to 3, caching 2.
    await getDecryptionKey(entry, 3)
    expect(entry.skippedKeys.has(2)).toBe(true)

    const key = await getDecryptionKey(entry, 2)
    expect(key).not.toBeNull()
    expect(entry.skippedKeys.has(2)).toBe(false)
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

// ── Worker decryptFrame — stale-key deduplication (regression) ─────────
//
// Regression test for the bug where `setKey` accumulated multiple chain
// entries for the SAME keyId, and `entries.find(e => (e.keyId & 0xff) ===
// kid)` picked the FIRST (stale) one, causing decryption to fail with a
// black tile. The fix filters out same-keyId entries before pushing, so
// only the LATEST key material for a given keyId is retained.
//
// Test strategy:
//   1. Install a stale key K1 at keyId=0.
//   2. Re-install a current key K2 at the same keyId=0 (re-delivery, reuse).
//   3. Build a real AES-GCM frame encrypted with K2.
//   4. Route it through the worker's decrypt transform.
//   5. Assert the frame was enqueued (decrypted successfully).
//      — With old push-always behaviour entries.find() returns K1 → decrypt
//        throws → frame is dropped and no enqueue occurs.
//      — With the fix K2 is the sole entry → decrypt succeeds → enqueued.

describe('e2ee-worker decryptFrame — stale same-keyId replacement', () => {
  let originalPostMessage: typeof self.postMessage
  const postedMessages: unknown[] = []

  beforeEach(() => {
    originalPostMessage = self.postMessage
    postedMessages.length = 0
    ;(self as unknown as Worker).postMessage = (msg: unknown) => {
      postedMessages.push(msg)
    }
  })

  afterEach(() => {
    ;(self as unknown as Worker).postMessage = originalPostMessage
  })

  it('decrypts a frame with the LATEST key when the same keyId is delivered twice with different material', async () => {
    await import('@/workers/e2ee-worker')

    // Unique sender so module-level state from other tests does not bleed in.
    const senderId = `test-sender-dedup-${Math.random().toString(36).slice(2)}`
    const keyId = 0

    // ── Generate two different keys with the same keyId ──────────────────
    const rawKeyK1 = await generateRawKey()
    const rawKeyK2 = await generateRawKey()

    // Helper: dispatch a setKey message and wait for the async handler to finish.
    async function dispatchSetKey(rawKey: ArrayBuffer) {
      await new Promise<void>(resolve => {
        self.dispatchEvent(
          Object.assign(new MessageEvent('message', {
            data: { type: 'setKey', participantId: senderId, keyId, rawKey },
          }), {}),
        )
        // Give the async initChainEntry inside the handler time to complete.
        Promise.resolve().then(() => setTimeout(resolve, 50))
      })
    }

    // Install K1 first (stale), then K2 (current). After the fix, only K2
    // survives in the senderChains entry for this sender.
    await dispatchSetKey(rawKeyK1)
    await dispatchSetKey(rawKeyK2)

    // ── Build a real encrypted frame using K2 ────────────────────────────
    // We use the same chain-entry helpers the worker itself uses so the frame
    // format matches exactly what decryptFrame expects.
    const encEntry = await initChainEntry(rawKeyK2, keyId)
    const ctr = 1 // first frame counter
    const plaintext = new TextEncoder().encode('regression-stale-key')

    // Derive the per-frame message key (advances chain, mirrors encryptFrame).
    const encKey = await getEncryptionKey(encEntry)
    const iv = computeNonce(encEntry.baseSalt, ctr)

    // Build the E2EE header: [KID 1B][CTR 4B big-endian]
    const e2eeHeader = new Uint8Array(E2EE_HEADER_LENGTH)
    e2eeHeader[0] = keyId & 0xff
    new DataView(e2eeHeader.buffer).setUint32(HEADER_KID_LENGTH, ctr)

    // No codec header for audio (payloadType=111 → 0 unencrypted bytes).
    const codecHeader = new Uint8Array(0)
    const aad = buildAAD(senderId, codecHeader, e2eeHeader)

    const ciphertext = await crypto.subtle.encrypt(
      { name: 'AES-GCM', iv, additionalData: aad, tagLength: GCM_TAG_LENGTH },
      encKey,
      plaintext,
    )

    // Frame layout: [e2eeHeader 5B][ciphertext+tag][trailer 1B = 0]
    const ciphertextBytes = new Uint8Array(ciphertext)
    const frameBuf = new ArrayBuffer(E2EE_HEADER_LENGTH + ciphertextBytes.byteLength + TRAILER_LENGTH)
    const frameView = new Uint8Array(frameBuf)
    frameView.set(e2eeHeader, 0)
    frameView.set(ciphertextBytes, E2EE_HEADER_LENGTH)
    frameView[E2EE_HEADER_LENGTH + ciphertextBytes.byteLength] = 0 // trailer: 0 unencrypted codec bytes

    const frame = {
      data: frameBuf,
      getMetadata: () => ({ payloadType: 111 }), // audio → 0 unencrypted bytes
    }

    // ── Drive the frame through the worker's decrypt transform ───────────
    const enqueuedFrames: unknown[] = []

    const readable = new ReadableStream({
      start(controller) {
        controller.enqueue(frame)
        controller.close()
      },
    })
    const writable = new WritableStream({
      write(chunk) {
        enqueuedFrames.push(chunk)
      },
    })

    self.dispatchEvent(
      Object.assign(new Event('rtctransform'), {
        transformer: {
          readable,
          writable,
          options: { operation: 'decrypt', senderId },
        },
      }),
    )

    // Wait for the async pipeline to fully drain.
    await new Promise(r => setTimeout(r, 200))

    // ── Assert ───────────────────────────────────────────────────────────
    // Exactly one frame should have been enqueued (decryption succeeded with K2).
    // With the old push-always behaviour, entries.find() would return K1 (stale),
    // AES-GCM decryption would throw, the frame would be dropped, and
    // enqueuedFrames would remain empty.
    expect(enqueuedFrames).toHaveLength(1)

    // Verify the decrypted plaintext is correct (belt-and-suspenders).
    const decryptedData = (enqueuedFrames[0] as { data: ArrayBuffer }).data
    const decryptedText = new TextDecoder().decode(decryptedData)
    expect(decryptedText).toBe('regression-stale-key')
  })
})

// ── Constants ───────────────────────────────────────────────────────────

describe('constants', () => {
  it('has correct values', () => {
    expect(GCM_TAG_LENGTH).toBe(128)
    expect(E2EE_HEADER_LENGTH).toBe(5)
    expect(TRAILER_LENGTH).toBe(1)
    expect(MAX_SKIP).toBe(256)
    expect(MAX_INITIAL_SKIP).toBe(30_000)
    expect(REPLAY_WINDOW_SIZE).toBe(128)
  })
})

// ── Worker decryptFrame — KID mismatch → not-ready ─────────────────────
//
// Tests that e2ee_not_ready is emitted when a frame arrives whose KID does
// not match any installed chain entry (rotation window: sender has switched
// to key N+1 but receiver only has key N).

describe('e2ee-worker decryptFrame KID mismatch', () => {
  // We drive the worker module's transform pipeline by:
  //  1. Spying on self.postMessage to capture e2ee_not_ready events
  //  2. Dispatching a fake 'message' event to install the sender key (setKey)
  //  3. Dispatching a fake 'rtctransform' event with a ReadableStream that
  //     emits our crafted frame (KID = installed_kid + 1)
  //  4. Waiting for the pipeline to process the frame
  //  5. Asserting e2ee_not_ready was posted

  // Build a minimal valid E2EE-framed buffer with the given KID byte.
  // Layout (0 unencrypted codec bytes, i.e. audio-style frame):
  //   [KID 1B] [CTR 4B big-endian] [fake-ciphertext 16B] [trailer 1B = 0]
  // Total = 22 bytes — passes the minSize check (HEADER 5 + GCM_TAG 16 + TRAILER 1).
  function buildFrameWithKid(kid: number, ctr = 1): ArrayBuffer {
    const buf = new ArrayBuffer(22)
    const view = new DataView(buf)
    // E2EE header at offset 0 (0 unencrypted bytes)
    view.setUint8(0, kid & 0xff)           // KID
    view.setUint32(1, ctr)                 // CTR big-endian
    // Bytes 5-20: fake ciphertext (16 bytes, all zeros — will not decrypt)
    // Byte 21: trailer = 0 (unencrypted codec header length)
    view.setUint8(21, 0)
    return buf
  }

  // Minimal RTCEncodedFrame mock
  function makeFrame(data: ArrayBuffer): { data: ArrayBuffer; getMetadata: () => { payloadType: number } } {
    return {
      data,
      getMetadata: () => ({ payloadType: 111 }), // audio payload type → 0 unencrypted bytes
    }
  }

  let originalPostMessage: typeof self.postMessage
  const postedMessages: unknown[] = []

  beforeEach(async () => {
    // Spy on self.postMessage before loading the worker module
    originalPostMessage = self.postMessage
    postedMessages.length = 0
    ;(self as unknown as Worker).postMessage = (msg: unknown) => {
      postedMessages.push(msg)
    }

    // Load (or re-use) the worker module. Because Vitest caches modules, the
    // worker's module-level senderChains map persists. We reset it by sending
    // a removeKeys message before each test.
  })

  afterEach(() => {
    ;(self as unknown as Worker).postMessage = originalPostMessage
  })

  it('emits e2ee_not_ready when the frame KID is not installed (rotation window)', async () => {
    // Import the worker module — this registers the 'message' and
    // 'rtctransform' event listeners on self.
    await import('@/workers/e2ee-worker')

    const senderId = `test-sender-kid-mismatch-${Math.random().toString(36).slice(2)}`

    // Install sender key at keyId = 0
    const rawKey = await generateRawKey()
    await new Promise<void>(resolve => {
      const handler = () => {
        self.removeEventListener('message', handler)
        resolve()
      }
      // The worker's message handler is async; we resolve after a micro-tick
      // by piggybacking on the event loop: post the message, then wait one
      // promise tick for the async handler to set the key.
      self.dispatchEvent(Object.assign(new MessageEvent('message', {
        data: { type: 'setKey', participantId: senderId, keyId: 0, rawKey },
      }), {}))
      // Give the async initChainEntry inside the handler time to complete
      Promise.resolve().then(() => setTimeout(resolve, 50))
    })

    // Now clear any not-ready notifications that may have fired (none expected
    // yet, but reset the spy to isolate the assertion below).
    postedMessages.length = 0

    // Build a frame whose KID byte is 1 (not installed — only keyId=0 exists)
    const frameData = buildFrameWithKid(1, 1)
    const frame = makeFrame(frameData)

    // Drive the frame through the worker's decrypt transform.
    // We construct a ReadableStream that emits the frame, then a writable sink,
    // and dispatch a synthetic 'rtctransform' event that the worker listens to.
    let framePushed: () => void
    const readable = new ReadableStream({
      start(controller) {
        // Push the frame immediately, then close
        controller.enqueue(frame)
        controller.close()
      },
    })
    const writable = new WritableStream({ write() {} })

    self.dispatchEvent(Object.assign(new Event('rtctransform'), {
      transformer: {
        readable,
        writable,
        options: { operation: 'decrypt', senderId },
      },
    }))

    // Wait for the pipeline to fully drain (the transform is async)
    await new Promise(r => setTimeout(r, 200))

    const notReadyEvents = postedMessages.filter(
      (m): m is { type: string; operation: string; participantId: string } =>
        typeof m === 'object' && m !== null && (m as Record<string, unknown>).type === 'e2ee_not_ready',
    )

    expect(notReadyEvents.length).toBeGreaterThanOrEqual(1)
    const evt = notReadyEvents[0]
    expect(evt.operation).toBe('decrypt')
    expect(evt.participantId).toBe(senderId)
  })
})

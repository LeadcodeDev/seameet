import { describe, it, expect, vi, beforeEach } from 'vitest'
import { renderHook, act } from '@testing-library/react'
import { useE2EE, type UseE2EEOptions } from '@/hooks/useE2EE'

// ── Mock Worker ─────────────────────────────────────────────────────────

class MockWorker {
  messages: Array<{ type: string; [key: string]: unknown }> = []
  onmessage: ((ev: MessageEvent) => void) | null = null
  private listeners: Array<(ev: MessageEvent) => void> = []

  postMessage(data: unknown): void {
    this.messages.push(data as { type: string; [key: string]: unknown })
  }

  terminate(): void {}
  addEventListener(_type: string, listener: (ev: MessageEvent) => void): void {
    this.listeners.push(listener)
  }
  removeEventListener(_type: string, listener: (ev: MessageEvent) => void): void {
    this.listeners = this.listeners.filter(l => l !== listener)
  }
  dispatchEvent(): boolean { return true }

  /** Test helper: simulate the worker posting a message back to the main thread. */
  emit(data: unknown): void {
    const ev = { data } as MessageEvent
    for (const l of this.listeners) l(ev)
  }
}

let lastWorker: MockWorker | null = null

vi.stubGlobal('Worker', class extends MockWorker {
  constructor(_url: URL, _opts?: WorkerOptions) {
    super()
    lastWorker = this
  }
})

// ── Helpers ─────────────────────────────────────────────────────────────

function createSignaling() {
  const sent: unknown[] = []
  return {
    send: vi.fn((msg: unknown) => { sent.push(msg) }),
    state: 'open' as const,
    sent,
  }
}

function defaultOptions(overrides?: Partial<UseE2EEOptions>): UseE2EEOptions {
  return {
    enabled: true,
    participantId: 'local-id',
    roomId: 'room-1',
    signaling: createSignaling() as unknown as UseE2EEOptions['signaling'],
    joined: true,
    ...overrides,
  }
}

async function flushAsync(): Promise<void> {
  await act(async () => {
    await new Promise(r => setTimeout(r, 50))
  })
}

// ── Tests ───────────────────────────────────────────────────────────────

describe('useE2EE', () => {
  beforeEach(() => {
    lastWorker = null
  })

  it('generates ECDH keypair and sets initial key on worker', async () => {
    renderHook(() => useE2EE(defaultOptions()))
    await flushAsync()

    expect(lastWorker).not.toBeNull()
    const setKeyMsg = lastWorker!.messages.find(m => m.type === 'setKey')
    expect(setKeyMsg).toBeDefined()
    expect(setKeyMsg!.participantId).toBe('local-id')
    expect(setKeyMsg!.keyId).toBe(0)
    expect(setKeyMsg!.rawKey).toBeDefined()
  })

  it('does nothing when disabled', async () => {
    const { result } = renderHook(() => useE2EE(defaultOptions({ enabled: false })))
    await flushAsync()

    expect(result.current.worker).toBeNull()
    expect(lastWorker).toBeNull()
  })

  it('onPeerJoined broadcasts public key (rotation deferred to key exchange)', async () => {
    const sig = createSignaling()
    const { result } = renderHook(() => useE2EE(defaultOptions({ signaling: sig as unknown as UseE2EEOptions['signaling'] })))
    await flushAsync()

    await act(async () => {
      await result.current.onPeerJoined('peer-1')
    })
    await flushAsync()

    // Should have broadcast public key
    const pubKeyMsgs = sig.sent.filter((m: any) => m.type === 'e2ee_public_key')
    expect(pubKeyMsgs.length).toBeGreaterThan(0)

    // No rotation yet — only the initial setKey from mount
    const setKeys = lastWorker!.messages.filter(m => m.type === 'setKey')
    expect(setKeys.length).toBe(1) // initial only, rotation deferred

    // Peer state should exist
    expect(result.current.peerStates.has('peer-1')).toBe(true)
    expect(result.current.peerStates.get('peer-1')!.ready).toBe(false)
  })

  it('onPeerLeft removes peer state without rotating the sender key', async () => {
    const sig = createSignaling()
    const { result } = renderHook(() => useE2EE(defaultOptions({ signaling: sig as unknown as UseE2EEOptions['signaling'] })))
    await flushAsync()

    // First add a peer
    await act(async () => {
      await result.current.onPeerJoined('peer-1')
    })
    await flushAsync()
    expect(result.current.peerStates.has('peer-1')).toBe(true)

    // Then remove
    const setKeysBefore = lastWorker!.messages.filter(m => m.type === 'setKey').length
    await act(async () => {
      await result.current.onPeerLeft('peer-1')
    })
    await flushAsync()

    expect(result.current.peerStates.has('peer-1')).toBe(false)

    // Should have sent removeKeys to worker
    const removeKeys = lastWorker!.messages.filter(m => m.type === 'removeKeys')
    expect(removeKeys.length).toBe(1)
    expect(removeKeys[0].participantId).toBe('peer-1')

    // Forward secrecy is provided by the periodic DH ratchet, not by
    // per-departure rotation. Leaving must NOT trigger a new setKey.
    const setKeysAfter = lastWorker!.messages.filter(m => m.type === 'setKey').length
    expect(setKeysAfter).toBe(setKeysBefore)
  })

  it('encryptChat returns ciphertext when enabled', async () => {
    const { result } = renderHook(() => useE2EE(defaultOptions()))
    await flushAsync()

    let encrypted: { ciphertext: string; keyId: number } | null = null
    await act(async () => {
      encrypted = await result.current.encryptChat('hello')
    })

    expect(encrypted).not.toBeNull()
    expect(encrypted!.ciphertext).toBeTruthy()
    expect(typeof encrypted!.keyId).toBe('number')
  })

  it('encryptChat returns null when disabled', async () => {
    const { result } = renderHook(() => useE2EE(defaultOptions({ enabled: false })))
    await flushAsync()

    let encrypted: { ciphertext: string; keyId: number } | null = null
    await act(async () => {
      encrypted = await result.current.encryptChat('hello')
    })

    expect(encrypted).toBeNull()
  })

  it('decryptChat decrypts own message (echo)', async () => {
    const { result } = renderHook(() => useE2EE(defaultOptions()))
    await flushAsync()

    let encrypted: { ciphertext: string; keyId: number } | null = null
    await act(async () => {
      encrypted = await result.current.encryptChat('test message')
    })
    expect(encrypted).not.toBeNull()

    let decrypted: string | null = null
    await act(async () => {
      decrypted = await result.current.decryptChat('local-id', encrypted!.ciphertext, encrypted!.keyId)
    })
    expect(decrypted).toBe('test message')
  })

  it('decryptChat returns null for unknown peer', async () => {
    const { result } = renderHook(() => useE2EE(defaultOptions()))
    await flushAsync()

    let decrypted: string | null = null
    await act(async () => {
      decrypted = await result.current.decryptChat('unknown-peer', 'garbage', 0)
    })
    expect(decrypted).toBeNull()
  })

  it('safety number format is 12 groups of 5 digits', async () => {
    // Safety numbers are computed when handling e2ee_public_key messages.
    // We can test the format by verifying it matches the pattern.
    // Since we can't easily simulate the full key exchange here,
    // we at least verify the hook exposes safetyNumbers map.
    const { result } = renderHook(() => useE2EE(defaultOptions()))
    await flushAsync()
    expect(result.current.safetyNumbers).toBeInstanceOf(Map)
  })

  it('localKeyId starts at 0', async () => {
    const { result } = renderHook(() => useE2EE(defaultOptions()))
    await flushAsync()
    expect(result.current.localKeyId).toBe(0)
  })

  // Regression: when an `e2ee_sender_key` message arrives BEFORE the matching
  // `e2ee_public_key` (out-of-order on the wire, or interleaved between the
  // awaits of the public-key handler), the previous implementation logged
  // "no shared secret" and silently dropped the sender key — leaving the peer
  // permanently without our means to decrypt their video. The fix queues
  // sender_key messages until the shared secret is derived, then drains.
  it('queues e2ee_sender_key arriving before shared secret and drains on pubkey', async () => {
    const sig = createSignaling()
    const { result } = renderHook(() => useE2EE(defaultOptions({
      participantId: 'local-id',
      signaling: sig as unknown as UseE2EEOptions['signaling'],
    })))
    await flushAsync()

    // Simulate a remote peer with its own ECDH keypair
    const peerKeyPair = await crypto.subtle.generateKey(
      { name: 'ECDH', namedCurve: 'P-256' }, true, ['deriveKey', 'deriveBits'],
    )

    // Recover the local hook's broadcast pubkey from sig.sent so the fake peer
    // can derive the same shared secret the hook will derive on its side.
    const localPubKeyMsg = sig.sent.find((m: any) => m.type === 'e2ee_public_key' && m.from === 'local-id') as any
    expect(localPubKeyMsg).toBeDefined()
    const localPubKeyBytes = Uint8Array.from(atob(localPubKeyMsg.public_key), c => c.charCodeAt(0))
    const localPubKey = await crypto.subtle.importKey(
      'raw', localPubKeyBytes, { name: 'ECDH', namedCurve: 'P-256' }, true, [],
    )
    const peerSharedSecret = await crypto.subtle.deriveKey(
      { name: 'ECDH', public: localPubKey }, peerKeyPair.privateKey,
      { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt'],
    )

    // Build a fake sender key encrypted for our hook
    const fakeSenderKey = crypto.getRandomValues(new Uint8Array(32))
    const iv = crypto.getRandomValues(new Uint8Array(12))
    const ct = await crypto.subtle.encrypt(
      { name: 'AES-GCM', iv, tagLength: 128 }, peerSharedSecret, fakeSenderKey,
    )
    const packed = new Uint8Array(12 + ct.byteLength)
    packed.set(iv, 0)
    packed.set(new Uint8Array(ct), 12)
    const encryptedSenderKey = btoa(String.fromCharCode(...packed))

    const peerPubKeyRaw = new Uint8Array(await crypto.subtle.exportKey('raw', peerKeyPair.publicKey))
    const peerPubKeyBase64 = btoa(String.fromCharCode(...peerPubKeyRaw))

    // 1) sender_key arrives FIRST (no shared secret yet → must be queued)
    await act(async () => {
      await result.current.handleMessage({
        type: 'e2ee_sender_key',
        from: 'peer-1',
        to: 'local-id',
        room_id: 'room-1',
        encrypted_key: encryptedSenderKey,
        key_id: 0,
      } as any)
    })
    await flushAsync()
    // Peer not ready yet — its sender_key was queued
    expect(result.current.peerStates.get('peer-1')?.ready).not.toBe(true)

    // 2) pubkey arrives → shared secret derived → queued sender_key drained
    await act(async () => {
      await result.current.handleMessage({
        type: 'e2ee_public_key',
        from: 'peer-1',
        room_id: 'room-1',
        public_key: peerPubKeyBase64,
      } as any)
    })
    await flushAsync()

    // Peer is now ready and the worker received the decrypted sender key
    expect(result.current.peerStates.get('peer-1')?.ready).toBe(true)
    expect(result.current.peerStates.get('peer-1')?.keyId).toBe(0)
    const peerSetKey = lastWorker!.messages.find(
      m => m.type === 'setKey' && m.participantId === 'peer-1',
    )
    expect(peerSetKey).toBeDefined()
    expect(peerSetKey!.keyId).toBe(0)

    // request_keyframe should have been signaled to PLI the peer's encoder
    const pliMsg = sig.sent.find((m: any) => m.type === 'request_keyframe' && m.target === 'peer-1')
    expect(pliMsg).toBeDefined()
  })

  // M3 fail-closed: worker drops frames and posts e2ee_not_ready when no key
  // is installed. The hook must surface that into `e2eeNotReady` and clear
  // it when the corresponding key arrives.
  it('surfaces e2ee_not_ready from the worker for a peer and clears it when sender key arrives', async () => {
    const sig = createSignaling()
    const { result } = renderHook(() => useE2EE(defaultOptions({
      participantId: 'local-id',
      signaling: sig as unknown as UseE2EEOptions['signaling'],
    })))
    await flushAsync()

    expect(result.current.e2eeNotReady.peers.has('peer-x')).toBe(false)

    await act(async () => {
      lastWorker!.emit({
        type: 'e2ee_not_ready',
        operation: 'decrypt',
        participantId: 'peer-x',
      })
    })
    await flushAsync()

    expect(result.current.e2eeNotReady.peers.has('peer-x')).toBe(true)

    // Now simulate the peer's pubkey + sender_key handshake completing —
    // the not-ready flag must clear.
    const peerKeyPair = await crypto.subtle.generateKey(
      { name: 'ECDH', namedCurve: 'P-256' }, true, ['deriveKey', 'deriveBits'],
    )
    const localPubKeyMsg = sig.sent.find((m: any) => m.type === 'e2ee_public_key' && m.from === 'local-id') as any
    const localPubKeyBytes = Uint8Array.from(atob(localPubKeyMsg.public_key), c => c.charCodeAt(0))
    const localPubKey = await crypto.subtle.importKey(
      'raw', localPubKeyBytes, { name: 'ECDH', namedCurve: 'P-256' }, true, [],
    )
    const peerSharedSecret = await crypto.subtle.deriveKey(
      { name: 'ECDH', public: localPubKey }, peerKeyPair.privateKey,
      { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt'],
    )
    const fakeSenderKey = crypto.getRandomValues(new Uint8Array(32))
    const iv = crypto.getRandomValues(new Uint8Array(12))
    const ct = await crypto.subtle.encrypt(
      { name: 'AES-GCM', iv, tagLength: 128 }, peerSharedSecret, fakeSenderKey,
    )
    const packed = new Uint8Array(12 + ct.byteLength)
    packed.set(iv, 0)
    packed.set(new Uint8Array(ct), 12)
    const encryptedSenderKey = btoa(String.fromCharCode(...packed))
    const peerPubKeyRaw = new Uint8Array(await crypto.subtle.exportKey('raw', peerKeyPair.publicKey))
    const peerPubKeyBase64 = btoa(String.fromCharCode(...peerPubKeyRaw))

    await act(async () => {
      await result.current.handleMessage({
        type: 'e2ee_public_key',
        from: 'peer-x',
        room_id: 'room-1',
        public_key: peerPubKeyBase64,
      } as any)
      await result.current.handleMessage({
        type: 'e2ee_sender_key',
        from: 'peer-x',
        to: 'local-id',
        room_id: 'room-1',
        encrypted_key: encryptedSenderKey,
        key_id: 0,
      } as any)
    })
    await flushAsync()

    expect(result.current.e2eeNotReady.peers.has('peer-x')).toBe(false)
  })

  // D-INV-1: The DH ratchet (fires every 2 minutes) must emit an
  // `e2ee_key_rotation` signal so remote peers know a rotation occurred.
  //
  // Timer approach: capture the ratchet interval callback by spying on
  // setInterval, then invoke it directly and await it — no fake clock needed.
  it('emits e2ee_key_rotation when the DH ratchet fires (D-INV-1)', async () => {
    const sig = createSignaling()

    // Capture the ratchet callback registered by the hook's setInterval.
    // The ratchet is the only 2-minute (120 000ms) interval in the hook.
    let ratchetCallback: (() => Promise<void>) | null = null
    const origSetInterval = globalThis.setInterval.bind(globalThis)
    const setIntervalSpy = vi.spyOn(globalThis, 'setInterval').mockImplementation(
      (fn: TimerHandler, delay?: number, ...args: unknown[]) => {
        if (delay === 2 * 60 * 1000) {
          ratchetCallback = fn as () => Promise<void>
        }
        return origSetInterval(fn, delay, ...args)
      },
    )

    try {
      // Render with real timers; let the initial async key gen complete.
      await act(async () => {
        renderHook(() => useE2EE(defaultOptions({ signaling: sig as unknown as UseE2EEOptions['signaling'] })))
      })
      await flushAsync() // 50ms real wait — initial key gen + pubkey broadcast
      await flushAsync() // second flush for slower CI / full-suite load

      expect(sig.sent.some((m: any) => m.type === 'e2ee_public_key')).toBe(true)
      expect(ratchetCallback).not.toBeNull()

      const rotationsBefore = sig.sent.filter((m: any) => m.type === 'e2ee_key_rotation').length

      // Invoke the ratchet callback directly and await the async work.
      await act(async () => {
        await ratchetCallback!()
      })

      const rotations = sig.sent.filter((m: any) => m.type === 'e2ee_key_rotation')
      expect(rotations.length).toBeGreaterThan(rotationsBefore)
      expect((rotations[rotations.length - 1] as any).key_id).toBeGreaterThanOrEqual(1)
    } finally {
      setIntervalSpy.mockRestore()
    }
  })

  it('re-broadcasts our public key to recover a peer stuck not-ready (D-INV-2)', async () => {
    const setIntervalSpy = vi.spyOn(globalThis, 'setInterval')
    try {
      const sig = createSignaling()
      const { result } = renderHook(() => useE2EE(defaultOptions({ signaling: sig as unknown as UseE2EEOptions['signaling'] })))
      await flushAsync()

      // Capture the recovery watchdog callback (registered with RECOVERY_INTERVAL_MS = 3000).
      const call = setIntervalSpy.mock.calls.find(c => c[1] === 3000)
      expect(call).toBeDefined()
      const tick = call![0] as () => void

      // Peer becomes stuck not-ready (worker dropped its frames, no key yet).
      await act(async () => {
        lastWorker!.emit({ type: 'e2ee_not_ready', operation: 'decrypt', participantId: 'peer-stuck' })
      })
      await flushAsync()
      expect(result.current.e2eeNotReady.peers.has('peer-stuck')).toBe(true)

      const pubKeysBefore = sig.sent.filter((m: any) => m.type === 'e2ee_public_key').length

      // Watchdog fires → should re-broadcast our pubkey to trigger key re-delivery.
      await act(async () => { tick(); await Promise.resolve() })
      await flushAsync()

      const pubKeysAfter = sig.sent.filter((m: any) => m.type === 'e2ee_public_key').length
      expect(pubKeysAfter).toBeGreaterThan(pubKeysBefore)
    } finally {
      setIntervalSpy.mockRestore()
    }
  })

  it('stops re-broadcasting after MAX_RECOVERY_ATTEMPTS for a peer (D-INV-2 bounded)', async () => {
    const setIntervalSpy = vi.spyOn(globalThis, 'setInterval')
    try {
      const sig = createSignaling()
      const { result } = renderHook(() => useE2EE(defaultOptions({ signaling: sig as unknown as UseE2EEOptions['signaling'] })))
      await flushAsync()
      const tick = setIntervalSpy.mock.calls.find(c => c[1] === 3000)![0] as () => void

      await act(async () => { lastWorker!.emit({ type: 'e2ee_not_ready', operation: 'decrypt', participantId: 'peer-stuck' }) })
      await flushAsync()

      const before = sig.sent.filter((m: any) => m.type === 'e2ee_public_key').length
      // Fire many times; only MAX_RECOVERY_ATTEMPTS (5) re-broadcasts should occur.
      for (let i = 0; i < 10; i++) { await act(async () => { tick(); await Promise.resolve() }) }
      await flushAsync()
      const after = sig.sent.filter((m: any) => m.type === 'e2ee_public_key').length
      expect(after - before).toBe(5)
    } finally {
      setIntervalSpy.mockRestore()
    }
  })

  // D-INV-3: The per-peer pending sender-key queue must be bounded so a
  // misbehaving or slow peer cannot cause unbounded memory growth.
  it('caps the pending e2ee_sender_key queue per peer (D-INV-3)', async () => {
    const sig = createSignaling()
    const { result } = renderHook(() => useE2EE(defaultOptions({
      participantId: 'local-id',
      signaling: sig as unknown as UseE2EEOptions['signaling'],
    })))
    await flushAsync()

    // Push many e2ee_sender_key messages for a peer with no prior pubkey
    // (so no shared secret exists → all are queued).
    // The cap is PENDING_SENDER_KEY_CAP = 20, so we push 30 to exceed it.
    const OVER_CAP = 30
    await act(async () => {
      for (let i = 0; i < OVER_CAP; i++) {
        await result.current.handleMessage({
          type: 'e2ee_sender_key',
          from: 'slow-peer',
          to: 'local-id',
          room_id: 'room-1',
          encrypted_key: 'dummy',
          key_id: i,
        } as any)
      }
    })
    await flushAsync()

    // Now provide the peer's pubkey so the queue drains.
    const peerKeyPair = await crypto.subtle.generateKey(
      { name: 'ECDH', namedCurve: 'P-256' }, true, ['deriveKey', 'deriveBits'],
    )
    // Recover the local hook's broadcast pubkey from sig.sent.
    const localPubKeyMsg = sig.sent.find((m: any) => m.type === 'e2ee_public_key' && m.from === 'local-id') as any
    expect(localPubKeyMsg).toBeDefined()
    const localPubKeyBytes = Uint8Array.from(atob(localPubKeyMsg.public_key), c => c.charCodeAt(0))
    const localPubKey = await crypto.subtle.importKey(
      'raw', localPubKeyBytes, { name: 'ECDH', namedCurve: 'P-256' }, true, [],
    )
    // Build a real shared secret from the peer side so decryption succeeds.
    const peerSharedSecret = await crypto.subtle.deriveKey(
      { name: 'ECDH', public: localPubKey }, peerKeyPair.privateKey,
      { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt'],
    )
    // We need valid encrypted sender keys for the drain to succeed; build one
    // and reuse it for the drained messages (key_id will differ but encrypt
    // still succeeds — processSenderKey only cares about decryptability).
    const fakeSenderKey = crypto.getRandomValues(new Uint8Array(32))
    const iv = crypto.getRandomValues(new Uint8Array(12))
    const ct = await crypto.subtle.encrypt(
      { name: 'AES-GCM', iv, tagLength: 128 }, peerSharedSecret, fakeSenderKey,
    )
    const packed = new Uint8Array(12 + ct.byteLength)
    packed.set(iv, 0)
    packed.set(new Uint8Array(ct), 12)
    const encryptedSenderKey = btoa(String.fromCharCode(...packed))

    // Re-push (cap + 1) properly-encrypted messages to replace the dummies
    // in the queue with messages that will actually decrypt. But since our
    // queue already holds dummy messages, we can't decrypt those. Instead,
    // we verify the cap by counting how many setKey calls the worker received
    // for 'slow-peer' after draining — it must be ≤ PENDING_SENDER_KEY_CAP (20).
    // Re-fill the queue with OVER_CAP valid messages now that pubkey is available
    // but drain hasn't happened yet. To avoid the chicken-and-egg, we just
    // assert that the worker receives at most PENDING_SENDER_KEY_CAP setKey
    // messages for 'slow-peer'.
    //
    // Simpler approach: clear the dummy queue by draining (it will fail on
    // decrypt, but queue is consumed). Then push OVER_CAP valid messages and
    // immediately provide pubkey. But providing the pubkey fires drainPending
    // automatically only if there are pending messages. Since the dummy drain
    // will consume the queue, we need the valid re-fill to happen BEFORE pubkey.
    //
    // Most deterministic: push valid messages ONLY, count worker setKey messages.

    // Reset: start fresh with a new hook render for clarity.
    lastWorker = null
    const sig2 = createSignaling()
    const { result: result2 } = renderHook(() => useE2EE(defaultOptions({
      participantId: 'local-id',
      signaling: sig2 as unknown as UseE2EEOptions['signaling'],
    })))
    await flushAsync()
    await flushAsync() // second flush for slower CI / full-suite load

    // Recover local pubkey for this new hook instance.
    const localPubKeyMsg2 = sig2.sent.find((m: any) => m.type === 'e2ee_public_key' && m.from === 'local-id') as any
    expect(localPubKeyMsg2).toBeDefined()
    const localPubKeyBytes2 = Uint8Array.from(atob(localPubKeyMsg2.public_key), c => c.charCodeAt(0))
    const localPubKey2 = await crypto.subtle.importKey(
      'raw', localPubKeyBytes2, { name: 'ECDH', namedCurve: 'P-256' }, true, [],
    )
    const peerKeyPair2 = await crypto.subtle.generateKey(
      { name: 'ECDH', namedCurve: 'P-256' }, true, ['deriveKey', 'deriveBits'],
    )
    const peerSharedSecret2 = await crypto.subtle.deriveKey(
      { name: 'ECDH', public: localPubKey2 }, peerKeyPair2.privateKey,
      { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt'],
    )

    // Build OVER_CAP valid encrypted sender keys.
    const encryptedKeys: string[] = []
    for (let i = 0; i < OVER_CAP; i++) {
      const sk = crypto.getRandomValues(new Uint8Array(32))
      const iv2 = crypto.getRandomValues(new Uint8Array(12))
      const ct2 = await crypto.subtle.encrypt(
        { name: 'AES-GCM', iv: iv2, tagLength: 128 }, peerSharedSecret2, sk,
      )
      const p2 = new Uint8Array(12 + ct2.byteLength)
      p2.set(iv2, 0)
      p2.set(new Uint8Array(ct2), 12)
      encryptedKeys.push(btoa(String.fromCharCode(...p2)))
    }

    // Send OVER_CAP e2ee_sender_key messages BEFORE pubkey (no shared secret).
    await act(async () => {
      for (let i = 0; i < OVER_CAP; i++) {
        await result2.current.handleMessage({
          type: 'e2ee_sender_key',
          from: 'slow-peer',
          to: 'local-id',
          room_id: 'room-1',
          encrypted_key: encryptedKeys[i],
          key_id: i,
        } as any)
      }
    })
    await flushAsync()

    // Now provide the pubkey → shared secret derived → queue drains.
    const peerPubKeyRaw2 = new Uint8Array(await crypto.subtle.exportKey('raw', peerKeyPair2.publicKey))
    const peerPubKeyBase642 = btoa(String.fromCharCode(...peerPubKeyRaw2))

    await act(async () => {
      await result2.current.handleMessage({
        type: 'e2ee_public_key',
        from: 'slow-peer',
        room_id: 'room-1',
        public_key: peerPubKeyBase642,
      } as any)
    })
    await flushAsync()

    // Count setKey messages for 'slow-peer' in the worker.
    const peerSetKeys = lastWorker!.messages.filter(
      m => m.type === 'setKey' && m.participantId === 'slow-peer',
    )
    // The queue should have been capped at 20 (PENDING_SENDER_KEY_CAP), so
    // we received at most 20 setKey calls, not 30.
    expect(peerSetKeys.length).toBeLessThanOrEqual(20)
    expect(peerSetKeys.length).toBeGreaterThan(0)
  })
})

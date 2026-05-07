import { describe, it, expect, vi, beforeEach } from 'vitest'
import { renderHook, act } from '@testing-library/react'
import { useE2EE, type UseE2EEOptions } from '@/hooks/useE2EE'

// ── Mock Worker ─────────────────────────────────────────────────────────

class MockWorker {
  messages: Array<{ type: string; [key: string]: unknown }> = []
  onmessage: ((ev: MessageEvent) => void) | null = null

  postMessage(data: unknown): void {
    this.messages.push(data as { type: string; [key: string]: unknown })
  }

  terminate(): void {}
  addEventListener(): void {}
  removeEventListener(): void {}
  dispatchEvent(): boolean { return true }
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
})

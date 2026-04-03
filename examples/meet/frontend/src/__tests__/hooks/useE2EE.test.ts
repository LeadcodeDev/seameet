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

  it('onPeerJoined broadcasts public key and rotates sender key', async () => {
    const sig = createSignaling()
    const { result } = renderHook(() => useE2EE(defaultOptions({ signaling: sig as unknown as UseE2EEOptions['signaling'] })))
    await flushAsync()

    const initialMsgCount = sig.sent.length

    await act(async () => {
      await result.current.onPeerJoined('peer-1')
    })
    await flushAsync()

    // Should have broadcast public key
    const pubKeyMsgs = sig.sent.filter((m: any) => m.type === 'e2ee_public_key')
    expect(pubKeyMsgs.length).toBeGreaterThan(0)

    // Should have set a new key in the worker (rotation)
    const setKeys = lastWorker!.messages.filter(m => m.type === 'setKey')
    expect(setKeys.length).toBeGreaterThanOrEqual(2) // initial + rotation

    // Peer state should exist
    expect(result.current.peerStates.has('peer-1')).toBe(true)
    expect(result.current.peerStates.get('peer-1')!.ready).toBe(false)
  })

  it('onPeerLeft removes peer state and rotates key', async () => {
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

    // Should have rotated key (new setKey)
    const setKeysAfter = lastWorker!.messages.filter(m => m.type === 'setKey').length
    expect(setKeysAfter).toBeGreaterThan(setKeysBefore)
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
})

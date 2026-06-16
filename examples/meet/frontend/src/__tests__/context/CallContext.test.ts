import { describe, it, expect, vi } from 'vitest'
import { renderHook, act } from '@testing-library/react'
import { createElement, type ReactNode } from 'react'
import { MemoryRouter } from 'react-router-dom'
import { CallProvider, useCall } from '@/context/CallContext'
import { getLastMockWebSocket } from '../mocks/mock-websocket'
import type { SignalingMessage } from '@/types'

function flushAsync(ms = 30): Promise<void> {
  return act(async () => {
    await new Promise(resolve => setTimeout(resolve, ms))
  })
}

interface WrapperOptions {
  roomId?: string
  displayName?: string
  participantId?: string
  initialAudioEnabled?: boolean
  initialVideoEnabled?: boolean
}

function createWrapper(opts: WrapperOptions | string = {}) {
  // Support legacy call signature: createWrapper(roomId, displayName, participantId)
  const options: WrapperOptions = typeof opts === 'string' ? { roomId: opts } : opts
  const {
    roomId = 'test-room',
    displayName = 'Alice',
    participantId = 'p1',
    initialAudioEnabled,
    initialVideoEnabled,
  } = options
  return function Wrapper({ children }: { children: ReactNode }) {
    return createElement(
      MemoryRouter,
      { initialEntries: [`/room/${roomId}`] },
      createElement(
        CallProvider,
        {
          participantId,
          displayName,
          roomId,
          authToken: 'test-token',
          initialAudioEnabled,
          initialVideoEnabled,
        },
        children
      )
    )
  }
}

async function setupCall(opts: WrapperOptions = {}) {
  const roomId = opts.roomId ?? 'test-room'
  const wrapper = createWrapper(opts)
  const hook = renderHook(() => useCall(), { wrapper })

  // Wait for WS to open + join to fire
  await flushAsync(50)

  const ws = getLastMockWebSocket()

  // Simulate server sending 'ready' (the WS auto-opened and join was sent)
  await act(async () => {
    ws.serverPush({
      type: 'ready',
      room_id: roomId,
      initiator: true,
      peers: [],
    })
    await new Promise(resolve => setTimeout(resolve, 30))
  })

  return { hook, ws }
}

describe('CallContext', () => {
  it('A joins — sends join message with initial mute signals', async () => {
    const wrapper = createWrapper()
    renderHook(() => useCall(), { wrapper })

    await flushAsync(50)

    const ws = getLastMockWebSocket()
    const msgs = ws.drain()

    const joinMsg = msgs.find(m => m.type === 'join')
    expect(joinMsg).toBeDefined()
    expect(joinMsg).toMatchObject({
      type: 'join',
      participant: 'p1',
      room_id: 'test-room',
      display_name: 'Alice',
    })

    // Initial mute signals sent right after join
    expect(msgs.find(m => m.type === 'mute_video')).toBeDefined()
    expect(msgs.find(m => m.type === 'mute_audio')).toBeDefined()
  })

  it('A joins, B joins — A sees B via room_status', async () => {
    const { hook, ws } = await setupCall()

    // Answer the initial offer
    await act(async () => {
      ws.serverPush({
        type: 'answer',
        from: 'server',
        to: 'p1',
        room_id: 'test-room',
        sdp: 'mock-answer',
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    // Server sends room_status with B
    await act(async () => {
      ws.serverPush({
        type: 'room_status',
        room_id: 'test-room',
        participants: [
          { id: 'p1', audio_muted: false, video_muted: false, screen_sharing: false },
          { id: 'peer-b', display_name: 'Bob', audio_muted: false, video_muted: false, screen_sharing: false },
        ],
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    expect(hook.result.current.remotePeers.has('peer-b')).toBe(true)
    expect(hook.result.current.remotePeers.get('peer-b')?.displayName).toBe('Bob')
  })

  it('C joins room with A and B already present', async () => {
    const wrapper = createWrapper({ roomId: 'room-abc', displayName: 'Charlie', participantId: 'p-charlie' })
    const hook = renderHook(() => useCall(), { wrapper })

    await flushAsync(50)
    const ws = getLastMockWebSocket()

    // Server sends ready with two existing peers
    await act(async () => {
      ws.serverPush({
        type: 'ready',
        room_id: 'room-abc',
        initiator: true,
        peers: ['peer-a', 'peer-b'],
        display_names: { 'peer-a': 'Alice', 'peer-b': 'Bob' },
      })
      await new Promise(resolve => setTimeout(resolve, 30))
    })

    expect(hook.result.current.remotePeers.size).toBe(2)
    expect(hook.result.current.remotePeers.get('peer-a')?.displayName).toBe('Alice')
    expect(hook.result.current.remotePeers.get('peer-b')?.displayName).toBe('Bob')
  })

  it('toggleAudio sends unmute_audio then mute_audio', async () => {
    const { hook, ws } = await setupCall()

    // Drain initial mute signals sent on join
    ws.drain()

    // Toggle to unmute (starts muted)
    await act(async () => {
      await hook.result.current.toggleAudio()
    })
    await flushAsync()

    let msgs = ws.drain()
    expect(msgs.find(m => m.type === 'unmute_audio')).toBeDefined()

    // Toggle to mute
    await act(async () => {
      await hook.result.current.toggleAudio()
    })
    await flushAsync()

    msgs = ws.drain()
    expect(msgs.find(m => m.type === 'mute_audio')).toBeDefined()
  })

  it('updateVideoSettings sends video_config_changed', async () => {
    const { hook, ws } = await setupCall()

    act(() => {
      hook.result.current.updateVideoSettings({ width: 1280, height: 720, frameRate: 30 })
    })
    await flushAsync()

    const msgs = ws.drain()
    const configMsg = msgs.find(m => m.type === 'video_config_changed')
    expect(configMsg).toBeDefined()
    expect(configMsg).toMatchObject({
      type: 'video_config_changed',
      width: 1280,
      height: 720,
      fps: 30,
    })
  })

  it('leave closes the WebSocket', async () => {
    const { hook, ws } = await setupCall()

    act(() => {
      hook.result.current.leave()
    })
    await flushAsync()

    expect(ws.readyState).toBe(WebSocket.CLOSED)
  })

  it('toggleVideo sends unmute_video / mute_video', async () => {
    const { hook, ws } = await setupCall()

    // Drain initial mute signals sent on join
    ws.drain()

    // Toggle to unmute (starts muted)
    await act(async () => {
      await hook.result.current.toggleVideo()
    })
    await flushAsync()

    let msgs = ws.drain()
    expect(msgs.find(m => m.type === 'unmute_video')).toBeDefined()

    // Toggle to mute
    await act(async () => {
      await hook.result.current.toggleVideo()
    })
    await flushAsync()

    msgs = ws.drain()
    expect(msgs.find(m => m.type === 'mute_video')).toBeDefined()
  })

  it('signalingState reflects open', async () => {
    const wrapper = createWrapper()
    const hook = renderHook(() => useCall(), { wrapper })

    await flushAsync(50)

    expect(hook.result.current.signalingState).toBe('open')
  })

  it('reconnecting becomes true after the WS drops post-open', async () => {
    const { hook, ws } = await setupCall()
    expect(hook.result.current.signalingState).toBe('open')
    expect(hook.result.current.reconnecting).toBe(false)

    await act(async () => {
      ws.close()
      await new Promise(resolve => setTimeout(resolve, 30))
    })

    expect(hook.result.current.signalingState).not.toBe('open')
    expect(hook.result.current.reconnecting).toBe(true)
  })

  it('inbound error 401 surfaces fatalError', async () => {
    const { hook, ws } = await setupCall()
    expect(hook.result.current.fatalError).toBeNull()

    await act(async () => {
      ws.serverPush({ type: 'error', code: 401, message: 'token rejected' })
      await new Promise(resolve => setTimeout(resolve, 30))
    })

    expect(hook.result.current.fatalError).toEqual({ code: 401, message: 'token rejected' })
  })

  it('inbound error 403 surfaces fatalError', async () => {
    const { hook, ws } = await setupCall()

    await act(async () => {
      ws.serverPush({ type: 'error', code: 403, message: 'e2ee_required' })
      await new Promise(resolve => setTimeout(resolve, 30))
    })

    expect(hook.result.current.fatalError).toEqual({ code: 403, message: 'e2ee_required' })
  })

  it('duplicate chat messages (same id) are not appended twice', async () => {
    const { hook } = await setupCall()

    const chatMsg = {
      type: 'chat_message' as const,
      from: 'peer-b',
      room_id: 'test-room',
      content: 'hello',
      display_name: 'Bob',
      timestamp: 1000000,
    } as SignalingMessage

    // Re-fetch the current socket immediately before pushing — guards against
    // a prior-test reconnect having replaced the captured ws reference.
    const currentWs = getLastMockWebSocket()

    // Deliver the same message twice (simulating server replay on reconnect)
    await act(async () => {
      currentWs.serverPush(chatMsg)
      currentWs.serverPush(chatMsg)
      await new Promise(resolve => setTimeout(resolve, 50))
    })

    expect(hook.result.current.chatMessages).toHaveLength(1)
    expect(hook.result.current.chatMessages[0]).toMatchObject({
      id: 'peer-b-1000000',
      from: 'peer-b',
      content: 'hello',
    })
  })

  it('fatalError is cleared when signaling reconnects', async () => {
    const { hook } = await setupCall()

    // Re-fetch the current socket immediately before pushing — guards against
    // a stale reference if a prior-test reconnect ran between setupCall and here.
    const ws = getLastMockWebSocket()

    // Trigger a fatal error; poll until the state propagates rather than
    // sleeping a fixed interval (robust under scheduler load in the full suite).
    await act(async () => {
      ws.serverPush({ type: 'error', code: 401, message: 'token rejected' })
    })
    await vi.waitFor(
      () => expect(hook.result.current.fatalError).toEqual({ code: 401, message: 'token rejected' }),
      { timeout: 2000 },
    )

    // Simulate WS drop — poll until state reflects 'closed'.
    await act(async () => {
      ws.close()
    })
    await vi.waitFor(
      () => expect(hook.result.current.signalingState).toBe('closed'),
      { timeout: 2000 },
    )

    // Poll until the reconnect fires, the new WebSocket opens, and the hook
    // reports 'open' again — no hardcoded sleep for the 1 000 ms reconnect timer.
    await vi.waitFor(
      () => expect(hook.result.current.signalingState).toBe('open'),
      { timeout: 5000 },
    )
    expect(hook.result.current.fatalError).toBeNull()
  })
})

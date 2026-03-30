import { describe, it, expect, vi, beforeEach } from 'vitest'
import { renderHook, act } from '@testing-library/react'
import { createElement, type ReactNode } from 'react'
import { MemoryRouter } from 'react-router-dom'
import { CallProvider, useCall } from '@/context/CallContext'
import { getLastMockWebSocket, type MockWebSocket } from '../mocks/mock-websocket'
import type { SignalingMessage } from '@/types'

function flushAsync(ms = 30): Promise<void> {
  return act(async () => {
    await new Promise(resolve => setTimeout(resolve, ms))
  })
}

function createWrapper(roomId = 'test-room', displayName = 'Alice', participantId = 'p1') {
  return function Wrapper({ children }: { children: ReactNode }) {
    return createElement(
      MemoryRouter,
      { initialEntries: [`/room/${roomId}`] },
      createElement(
        CallProvider,
        { participantId, displayName, roomId },
        children
      )
    )
  }
}

async function setupCall(roomId = 'test-room') {
  const wrapper = createWrapper(roomId)
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
  it('A joins — sends join message', async () => {
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
  })

  it('A joins, B joins — A sees B in remotePeers', async () => {
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

    // B joins
    await act(async () => {
      ws.serverPush({
        type: 'peer_joined',
        participant: 'peer-b',
        room_id: 'test-room',
        display_name: 'Bob',
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    expect(hook.result.current.remotePeers.has('peer-b')).toBe(true)
    expect(hook.result.current.remotePeers.get('peer-b')?.displayName).toBe('Bob')
  })

  it('C joins room with A and B already present', async () => {
    const wrapper = createWrapper('room-abc', 'Charlie', 'p-charlie')
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

  it('toggleAudio sends mute_audio then unmute_audio', async () => {
    const { hook, ws } = await setupCall()

    // Toggle to mute
    act(() => {
      hook.result.current.toggleAudio()
    })
    await flushAsync()

    let msgs = ws.drain()
    expect(msgs.find(m => m.type === 'mute_audio')).toBeDefined()

    // Toggle to unmute
    act(() => {
      hook.result.current.toggleAudio()
    })
    await flushAsync()

    msgs = ws.drain()
    expect(msgs.find(m => m.type === 'unmute_audio')).toBeDefined()
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

  it('leave sends leave message', async () => {
    const { hook, ws } = await setupCall()

    act(() => {
      hook.result.current.leave()
    })
    await flushAsync()

    const msgs = ws.drain()
    const leaveMsg = msgs.find(m => m.type === 'leave')
    expect(leaveMsg).toBeDefined()
    expect(leaveMsg).toMatchObject({
      type: 'leave',
      participant: 'p1',
      room_id: 'test-room',
    })
  })

  it('toggleVideo sends mute_video / unmute_video', async () => {
    const { hook, ws } = await setupCall()

    act(() => {
      hook.result.current.toggleVideo()
    })
    await flushAsync()

    let msgs = ws.drain()
    expect(msgs.find(m => m.type === 'mute_video')).toBeDefined()

    act(() => {
      hook.result.current.toggleVideo()
    })
    await flushAsync()

    msgs = ws.drain()
    expect(msgs.find(m => m.type === 'unmute_video')).toBeDefined()
  })

  it('signalingState reflects open', async () => {
    const wrapper = createWrapper()
    const hook = renderHook(() => useCall(), { wrapper })

    await flushAsync(50)

    expect(hook.result.current.signalingState).toBe('open')
  })
})

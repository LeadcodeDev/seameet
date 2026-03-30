import { describe, it, expect, vi } from 'vitest'
import { renderHook, act } from '@testing-library/react'
import { useSignaling } from '@/hooks/useSignaling'
import { getLastMockWebSocket } from '../mocks/mock-websocket'
import { flushMicrotasks, drainMessages, serverPush } from '../helpers/test-ctx'
import type { SignalingMessage } from '@/types'

function renderSignaling(onMessage = vi.fn()) {
  return renderHook(() => useSignaling({ url: 'ws://test:3001', onMessage }))
}

describe('useSignaling', () => {
  it('connects and reports open state', async () => {
    const { result } = renderSignaling()
    expect(result.current.state).toBe('connecting')

    await flushMicrotasks()

    expect(result.current.state).toBe('open')
  })

  it('join() sends correct message', async () => {
    const { result } = renderSignaling()
    await flushMicrotasks()
    const ws = getLastMockWebSocket()

    act(() => {
      result.current.join('p1', 'room-1', 'Alice')
    })

    const msgs = drainMessages(ws)
    expect(msgs).toContainEqual({
      type: 'join',
      participant: 'p1',
      room_id: 'room-1',
      display_name: 'Alice',
    })
  })

  it('sendMuteAudio sends mute_audio message', async () => {
    const { result } = renderSignaling()
    await flushMicrotasks()
    const ws = getLastMockWebSocket()

    act(() => {
      result.current.sendMuteAudio('p1', 'room-1')
    })

    const msgs = drainMessages(ws)
    expect(msgs).toContainEqual({
      type: 'mute_audio',
      from: 'p1',
      room_id: 'room-1',
    })
  })

  it('sendUnmuteAudio sends unmute_audio message', async () => {
    const { result } = renderSignaling()
    await flushMicrotasks()
    const ws = getLastMockWebSocket()

    act(() => {
      result.current.sendUnmuteAudio('p1', 'room-1')
    })

    const msgs = drainMessages(ws)
    expect(msgs).toContainEqual({
      type: 'unmute_audio',
      from: 'p1',
      room_id: 'room-1',
    })
  })

  it('sendVideoConfig sends video_config_changed message', async () => {
    const { result } = renderSignaling()
    await flushMicrotasks()
    const ws = getLastMockWebSocket()

    act(() => {
      result.current.sendVideoConfig('p1', 'room-1', 1280, 720, 30)
    })

    const msgs = drainMessages(ws)
    expect(msgs).toContainEqual({
      type: 'video_config_changed',
      from: 'p1',
      room_id: 'room-1',
      width: 1280,
      height: 720,
      fps: 30,
    })
  })

  it('incoming messages dispatch to onMessage callback', async () => {
    const onMessage = vi.fn()
    renderSignaling(onMessage)
    await flushMicrotasks()
    const ws = getLastMockWebSocket()

    const readyMsg: SignalingMessage = {
      type: 'ready',
      room_id: 'room-1',
      initiator: true,
      peers: [],
    }

    await serverPush(ws, readyMsg)

    expect(onMessage).toHaveBeenCalledWith(readyMsg)
  })

  it('send is no-op when socket closed', async () => {
    const { result } = renderSignaling()
    await flushMicrotasks()
    const ws = getLastMockWebSocket()

    ws.close()

    // Should not throw
    act(() => {
      result.current.send({ type: 'mute_audio', from: 'p1', room_id: 'room-1' })
    })

    const msgs = drainMessages(ws)
    expect(msgs).toHaveLength(0)
  })
})

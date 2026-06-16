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

  it('queues a chat message sent while the socket is not open and flushes it after rejoin', async () => {
    const { result } = renderSignaling()
    await flushMicrotasks()
    const ws = getLastMockWebSocket()

    // Drive socket to non-OPEN state (close it)
    ws.close()

    // Drain any messages sent before close
    drainMessages(ws)

    // Send a chat message while socket is closed
    act(() => {
      result.current.sendChatMessage('p1', 'room-1', 'hello world', 'Alice')
    })

    // Assert the underlying ws.send was NOT yet called with the chat frame
    const msgsBeforeRejoin = drainMessages(ws)
    expect(msgsBeforeRejoin).toHaveLength(0)

    // Bring the socket back to OPEN state
    ws.readyState = ws.OPEN

    // Call join — this should send the join frame and then flush the queued chat
    act(() => {
      result.current.join('p1', 'room-1', 'Alice')
    })

    const msgsAfterRejoin = drainMessages(ws)

    // Both join and queued chat must have been sent
    const joinIdx = msgsAfterRejoin.findIndex(m => m.type === 'join')
    const chatIdx = msgsAfterRejoin.findIndex(m => m.type === 'chat_message')

    expect(joinIdx).toBeGreaterThanOrEqual(0)
    expect(chatIdx).toBeGreaterThanOrEqual(0)
    // Chat must come AFTER join
    expect(chatIdx).toBeGreaterThan(joinIdx)

    expect(msgsAfterRejoin[chatIdx]).toMatchObject({
      type: 'chat_message',
      from: 'p1',
      room_id: 'room-1',
      content: 'hello world',
      display_name: 'Alice',
    })
  })

  it('does not queue an offer (SDP regenerated on rejoin)', async () => {
    const { result } = renderSignaling()
    await flushMicrotasks()
    const ws = getLastMockWebSocket()

    // Drive socket to non-OPEN state
    ws.close()
    drainMessages(ws)

    // Call sendOffer while socket is closed
    act(() => {
      result.current.sendOffer('p1', 'room-1', 'v=0\r\n...')
    })

    // Bring socket back to OPEN
    ws.readyState = ws.OPEN

    // Call join
    act(() => {
      result.current.join('p1', 'room-1', 'Alice')
    })

    const msgs = drainMessages(ws)

    // The offer must NOT have been sent (neither immediately nor after rejoin)
    const offerMsg = msgs.find(m => m.type === 'offer')
    expect(offerMsg).toBeUndefined()
  })
})

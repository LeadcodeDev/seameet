import { renderHook, act } from '@testing-library/react'
import { getLastMockWebSocket, type MockWebSocket } from '../mocks/mock-websocket'
import type { SignalingMessage } from '@/types'

/**
 * Wait for a microtask to flush (WebSocket onopen fires on microtask).
 */
export function flushMicrotasks(): Promise<void> {
  return act(async () => {
    await new Promise(resolve => setTimeout(resolve, 0))
  })
}

/**
 * Get the MockWebSocket instance and flush microtasks so onopen fires.
 * Must be called AFTER renderHook that creates a WebSocket.
 */
export async function getOpenedWebSocket(): Promise<MockWebSocket> {
  await flushMicrotasks()
  return getLastMockWebSocket()
}

/**
 * Drain all messages sent by the hook through the MockWebSocket.
 */
export function drainMessages(ws: MockWebSocket): SignalingMessage[] {
  return ws.drain()
}

/**
 * Push a server message and flush so React state updates.
 */
export async function serverPush(ws: MockWebSocket, msg: SignalingMessage): Promise<void> {
  await act(async () => {
    ws.serverPush(msg)
    // Let async message processing complete
    await new Promise(resolve => setTimeout(resolve, 10))
  })
}

export { renderHook, act }

import { afterEach, beforeEach } from 'vitest'
import { installMockWebSocket, resetMockWebSocket } from './mocks/mock-websocket'
import { installMockRTC, resetMidCounter } from './mocks/mock-rtc'
import { installMockMedia } from './mocks/mock-media'

// Install global mocks before any test
installMockWebSocket()
installMockRTC()
installMockMedia()

beforeEach(() => {
  resetMidCounter()
})

afterEach(() => {
  resetMockWebSocket()
})

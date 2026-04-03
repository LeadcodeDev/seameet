import { afterEach, beforeEach } from 'vitest'
import { installMockWebSocket, resetMockWebSocket } from './mocks/mock-websocket'
import { installMockRTC, resetMidCounter } from './mocks/mock-rtc'
import { installMockMedia } from './mocks/mock-media'

// Install global mocks before any test
installMockWebSocket()
installMockRTC()
installMockMedia()

// jsdom does not implement HTMLMediaElement.play() / pause()
HTMLMediaElement.prototype.play = function () { return Promise.resolve() }
HTMLMediaElement.prototype.pause = function () {}

beforeEach(() => {
  resetMidCounter()
})

afterEach(() => {
  resetMockWebSocket()
})

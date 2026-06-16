import { afterEach, beforeEach } from 'vitest'
import { cleanup } from '@testing-library/react'
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
  // Unmount rendered hooks/components so useSignaling's cleanup runs — this
  // clears pending reconnect timers and closes sockets, preventing real timers
  // from leaking across tests (a prior test's reconnect could otherwise create
  // a new socket mid-test and break the next test's signaling assertions).
  cleanup()
  resetMockWebSocket()
})

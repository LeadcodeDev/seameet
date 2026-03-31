import { test, expect } from '@playwright/test'
import {
  joinRoomWithMedia,
  expectVideoState,
  startScreenShare,
  stopScreenShare,
  expectScreenShareVisible,
  expectScreenShareGone,
  toggleCamera,
} from './helpers/room'

test.describe('Screen share — multi-participant', () => {
  test('A shares screen — B sees the screen share tile', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()

    const room = `e2e-screen-2p-${Date.now()}`

    await joinRoomWithMedia(pageA, room, 'Alice', { camera: true, mic: true })
    await joinRoomWithMedia(pageB, room, 'Bob', { camera: true, mic: true })

    // Both see 2 video tiles
    await expect(pageA.locator('[data-testid="video-tile"][data-video="on"]')).toHaveCount(2, { timeout: 15_000 })
    await expect(pageB.locator('[data-testid="video-tile"][data-video="on"]')).toHaveCount(2, { timeout: 15_000 })

    // A starts screen share
    await startScreenShare(pageA)

    // A sees own screen share tile (local)
    await expectScreenShareVisible(pageA, 'Alice')
    // B sees Alice's screen share tile (remote)
    await expectScreenShareVisible(pageB, 'Alice')

    await ctxA.close()
    await ctxB.close()
  })

  test('A shares then stops — B sees tile appear then disappear', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()

    const room = `e2e-screen-stop-${Date.now()}`

    await joinRoomWithMedia(pageA, room, 'Alice', { camera: true, mic: true })
    await joinRoomWithMedia(pageB, room, 'Bob', { camera: true, mic: true })

    await expect(pageB.locator('[data-testid="video-tile"][data-video="on"]')).toHaveCount(2, { timeout: 15_000 })

    // A starts screen share
    await startScreenShare(pageA)
    await expectScreenShareVisible(pageB, 'Alice')

    // A stops screen share
    await stopScreenShare(pageA)

    // Screen share tile disappears for both
    await expectScreenShareGone(pageA, 'Alice')
    await expectScreenShareGone(pageB, 'Alice')

    // Regular video tiles still present
    await expect(pageA.locator('[data-testid="video-tile"][data-video="on"]')).toHaveCount(2, { timeout: 10_000 })
    await expect(pageB.locator('[data-testid="video-tile"][data-video="on"]')).toHaveCount(2, { timeout: 10_000 })

    await ctxA.close()
    await ctxB.close()
  })

  test('A shares screen — does not affect B camera state', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()

    const room = `e2e-screen-indep-${Date.now()}`

    await joinRoomWithMedia(pageA, room, 'Alice', { camera: true, mic: true })
    await joinRoomWithMedia(pageB, room, 'Bob', { camera: false, mic: true })

    // A sees own video + B placeholder
    await expectVideoState(pageA, 'Alice', 'on')
    await expectVideoState(pageA, 'Bob', 'off')

    // A starts screen share
    await startScreenShare(pageA)
    await expectScreenShareVisible(pageB, 'Alice')

    // B's camera state unchanged — still off
    await expectVideoState(pageA, 'Bob', 'off')
    await expectVideoState(pageB, 'Bob', 'off')

    // A's camera state unchanged — still on
    await expectVideoState(pageA, 'Alice', 'on')
    await expectVideoState(pageB, 'Alice', 'on')

    await ctxA.close()
    await ctxB.close()
  })

  test('3 participants — A shares screen, B and C both see it', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const ctxC = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()
    const pageC = await ctxC.newPage()

    const room = `e2e-screen-3p-${Date.now()}`

    await joinRoomWithMedia(pageA, room, 'Alice', { camera: true, mic: true })
    await joinRoomWithMedia(pageB, room, 'Bob', { camera: true, mic: true })
    await joinRoomWithMedia(pageC, room, 'Charlie', { camera: true, mic: true })

    // All see 3 video tiles
    await expect(pageA.locator('[data-testid="video-tile"][data-video="on"]')).toHaveCount(3, { timeout: 15_000 })
    await expect(pageB.locator('[data-testid="video-tile"][data-video="on"]')).toHaveCount(3, { timeout: 15_000 })
    await expect(pageC.locator('[data-testid="video-tile"][data-video="on"]')).toHaveCount(3, { timeout: 15_000 })

    // A starts screen share
    await startScreenShare(pageA)

    // All 3 see Alice's screen share tile
    await expectScreenShareVisible(pageA, 'Alice')
    await expectScreenShareVisible(pageB, 'Alice')
    await expectScreenShareVisible(pageC, 'Alice')

    // Total tiles: 3 cameras + 1 screen share = 4 "on" tiles visible to each
    await expect(pageB.locator('[data-testid="video-tile"][data-video="on"]')).toHaveCount(4, { timeout: 10_000 })
    await expect(pageC.locator('[data-testid="video-tile"][data-video="on"]')).toHaveCount(4, { timeout: 10_000 })

    await ctxA.close()
    await ctxB.close()
    await ctxC.close()
  })

  test('A shares screen, toggles camera OFF — screen share stays, camera tile becomes placeholder', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()

    const room = `e2e-screen-cam-toggle-${Date.now()}`

    await joinRoomWithMedia(pageA, room, 'Alice', { camera: true, mic: true })
    await joinRoomWithMedia(pageB, room, 'Bob', { camera: true, mic: true })

    await expect(pageB.locator('[data-testid="video-tile"][data-video="on"]')).toHaveCount(2, { timeout: 15_000 })

    // A starts screen share
    await startScreenShare(pageA)
    await expectScreenShareVisible(pageB, 'Alice')

    // A toggles camera OFF — screen share should persist
    await toggleCamera(pageA)

    // Alice's camera tile shows placeholder
    await expectVideoState(pageB, 'Alice', 'off')
    // Alice's screen share tile still visible
    await expectScreenShareVisible(pageB, 'Alice')

    // Bob's camera unaffected
    await expectVideoState(pageB, 'Bob', 'on')

    await ctxA.close()
    await ctxB.close()
  })

  test('A shares screen, B joins late — B sees screen share immediately', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()

    const room = `e2e-screen-late-join-${Date.now()}`

    // A joins and starts screen share BEFORE B joins
    await joinRoomWithMedia(pageA, room, 'Alice', { camera: true, mic: true })
    await expect(pageA.locator('[data-testid="video-tile"][data-video="on"]')).toHaveCount(1, { timeout: 10_000 })

    await startScreenShare(pageA)
    await expectScreenShareVisible(pageA, 'Alice')

    // B joins after screen share is active
    await joinRoomWithMedia(pageB, room, 'Bob', { camera: true, mic: true })

    // B sees Alice's camera + screen share + own camera
    await expectVideoState(pageB, 'Alice', 'on')
    await expectScreenShareVisible(pageB, 'Alice')
    await expectVideoState(pageB, 'Bob', 'on')

    await ctxA.close()
    await ctxB.close()
  })

  test('A shares screen then leaves — B screen share tile disappears', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()

    const room = `e2e-screen-leave-${Date.now()}`

    await joinRoomWithMedia(pageA, room, 'Alice', { camera: true, mic: true })
    await joinRoomWithMedia(pageB, room, 'Bob', { camera: true, mic: true })

    await expect(pageB.locator('[data-testid="video-tile"][data-video="on"]')).toHaveCount(2, { timeout: 15_000 })

    // A starts screen share
    await startScreenShare(pageA)
    await expectScreenShareVisible(pageB, 'Alice')

    // A leaves the room
    await pageA.click('[data-testid="btn-leave"]')

    // B: Alice's screen share and camera tile both gone
    await expectScreenShareGone(pageB, 'Alice')
    await expect(pageB.locator('[data-testid="video-tile"]')).toHaveCount(1, { timeout: 10_000 })

    await ctxA.close()
    await ctxB.close()
  })
})

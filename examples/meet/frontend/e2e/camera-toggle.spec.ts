import { test, expect } from '@playwright/test'
import { joinRoomWithMedia, countVideoTiles, countPlaceholderTiles, participantHasVideo } from './helpers/room'

test.describe('Camera toggle', () => {
  test('A joins alone — sees own camera', async ({ page }) => {
    const room = `e2e-cam-solo-on-${Date.now()}`
    await joinRoomWithMedia(page, room, 'Alice', { camera: true, mic: true })

    await expect(page.locator('video:not(.hidden)')).toHaveCount(1, { timeout: 10_000 })
    expect(await countPlaceholderTiles(page)).toBe(0)
  })

  test('A joins alone — sees own placeholder when camera off', async ({ page }) => {
    const room = `e2e-cam-solo-off-${Date.now()}`
    await joinRoomWithMedia(page, room, 'Alice', { camera: false, mic: true })

    await expect(page.locator('span.bg-primary')).toHaveCount(1, { timeout: 10_000 })
    expect(await countVideoTiles(page)).toBe(0)
  })

  test('A+B join with camera ON — both see each other video', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()

    const room = `e2e-cam-duo-on-${Date.now()}`

    await joinRoomWithMedia(pageA, room, 'Alice', { camera: true, mic: true })
    await joinRoomWithMedia(pageB, room, 'Bob', { camera: true, mic: true })

    await expect(pageA.locator('video:not(.hidden)')).toHaveCount(2, { timeout: 15_000 })
    await expect(pageB.locator('video:not(.hidden)')).toHaveCount(2, { timeout: 15_000 })

    await ctxA.close()
    await ctxB.close()
  })

  test('A camera ON, B camera OFF — mixed tiles', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()

    const room = `e2e-cam-mixed-${Date.now()}`

    await joinRoomWithMedia(pageA, room, 'Alice', { camera: true, mic: true })
    await joinRoomWithMedia(pageB, room, 'Bob', { camera: false, mic: true })

    // A sees own video + B's placeholder
    await expect(pageA.locator('video:not(.hidden)')).toHaveCount(1, { timeout: 15_000 })
    await expect(pageA.locator('span.bg-primary')).toHaveCount(1, { timeout: 5_000 })

    // B sees own placeholder + A's video
    await expect(pageB.locator('video:not(.hidden)')).toHaveCount(1, { timeout: 15_000 })
    await expect(pageB.locator('span.bg-primary')).toHaveCount(1, { timeout: 5_000 })

    await ctxA.close()
    await ctxB.close()
  })

  test('A toggles camera OFF — B sees placeholder', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()

    const room = `e2e-cam-toggle-off-${Date.now()}`

    await joinRoomWithMedia(pageA, room, 'Alice', { camera: true, mic: true })
    await joinRoomWithMedia(pageB, room, 'Bob', { camera: true, mic: true })

    // Both see 2 videos
    await expect(pageA.locator('video:not(.hidden)')).toHaveCount(2, { timeout: 15_000 })
    await expect(pageB.locator('video:not(.hidden)')).toHaveCount(2, { timeout: 15_000 })

    // A toggles camera OFF (button currently shows Video icon)
    await pageA.click('button:has(.lucide-video)')

    // B sees A's placeholder appear
    await expect(pageB.locator('span.bg-primary')).toHaveCount(1, { timeout: 5_000 })
    // A sees own placeholder
    await expect(pageA.locator('span.bg-primary')).toHaveCount(1, { timeout: 5_000 })

    await ctxA.close()
    await ctxB.close()
  })

  test('A toggles camera ON in room — B sees video (bug fix)', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()

    const room = `e2e-cam-toggle-on-${Date.now()}`

    // Both join with camera ON first
    await joinRoomWithMedia(pageA, room, 'Alice', { camera: true, mic: true })
    await joinRoomWithMedia(pageB, room, 'Bob', { camera: true, mic: true })

    // Both see 2 videos
    await expect(pageA.locator('video:not(.hidden)')).toHaveCount(2, { timeout: 15_000 })
    await expect(pageB.locator('video:not(.hidden)')).toHaveCount(2, { timeout: 15_000 })

    // A toggles camera OFF
    await pageA.click('button:has(.lucide-video)')
    await expect(pageB.locator('span.bg-primary')).toHaveCount(1, { timeout: 5_000 })

    // A toggles camera ON (the bug fix — replaceLocalTracks must be called)
    await pageA.click('button:has(.lucide-video-off)')

    // B should see A's video again — no more placeholders
    await expect(pageB.locator('span.bg-primary')).toHaveCount(0, { timeout: 10_000 })
    await expect(pageB.locator('video:not(.hidden)')).toHaveCount(2, { timeout: 10_000 })

    // A also sees own video
    await expect(pageA.locator('video:not(.hidden)')).toHaveCount(2, { timeout: 10_000 })

    await ctxA.close()
    await ctxB.close()
  })

  test('Three participants — C joins, B toggles OFF, C sees A video + B placeholder', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const ctxC = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()
    const pageC = await ctxC.newPage()

    const room = `e2e-cam-trio-${Date.now()}`

    // All join with camera ON
    await joinRoomWithMedia(pageA, room, 'Alice', { camera: true, mic: true })
    await joinRoomWithMedia(pageB, room, 'Bob', { camera: true, mic: true })
    await joinRoomWithMedia(pageC, room, 'Charlie', { camera: true, mic: true })

    // All see 3 videos
    await expect(pageC.locator('video:not(.hidden)')).toHaveCount(3, { timeout: 15_000 })

    // B toggles camera OFF
    await pageB.click('button:has(.lucide-video)')

    // C sees: own video + A's video + B's placeholder
    await expect(pageC.locator('video:not(.hidden)')).toHaveCount(2, { timeout: 5_000 })
    await expect(pageC.locator('span.bg-primary')).toHaveCount(1, { timeout: 5_000 })

    await ctxA.close()
    await ctxB.close()
    await ctxC.close()
  })

  test('A toggles OFF then ON — B and C see changes', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const ctxC = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()
    const pageC = await ctxC.newPage()

    const room = `e2e-cam-toggle-trio-${Date.now()}`

    await joinRoomWithMedia(pageA, room, 'Alice', { camera: true, mic: true })
    await joinRoomWithMedia(pageB, room, 'Bob', { camera: true, mic: true })
    await joinRoomWithMedia(pageC, room, 'Charlie', { camera: true, mic: true })

    // All see 3 videos
    await expect(pageA.locator('video:not(.hidden)')).toHaveCount(3, { timeout: 15_000 })
    await expect(pageB.locator('video:not(.hidden)')).toHaveCount(3, { timeout: 15_000 })
    await expect(pageC.locator('video:not(.hidden)')).toHaveCount(3, { timeout: 15_000 })

    // A toggles camera OFF
    await pageA.click('button:has(.lucide-video)')

    // B and C see A's placeholder
    await expect(pageB.locator('span.bg-primary')).toHaveCount(1, { timeout: 5_000 })
    await expect(pageC.locator('span.bg-primary')).toHaveCount(1, { timeout: 5_000 })

    // A toggles camera ON
    await pageA.click('button:has(.lucide-video-off)')

    // B and C see A's video again — no more placeholders
    await expect(pageB.locator('span.bg-primary')).toHaveCount(0, { timeout: 10_000 })
    await expect(pageC.locator('span.bg-primary')).toHaveCount(0, { timeout: 10_000 })
    await expect(pageB.locator('video:not(.hidden)')).toHaveCount(3, { timeout: 10_000 })
    await expect(pageC.locator('video:not(.hidden)')).toHaveCount(3, { timeout: 10_000 })

    await ctxA.close()
    await ctxB.close()
    await ctxC.close()
  })
})

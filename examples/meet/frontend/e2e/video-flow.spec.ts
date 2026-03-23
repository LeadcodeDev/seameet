import { test, expect } from '@playwright/test'
import { joinRoom, waitForVideoPlaying, countVideoTiles } from './helpers/room'

test.describe('Video flow', () => {
  test('A joins alone — sees own tile', async ({ page }) => {
    await joinRoom(page, 'e2e-solo', 'Alice')

    await expect(page.locator('video')).toHaveCount(1, { timeout: 10_000 })
    await waitForVideoPlaying(page, 'video')
  })

  test('A and B join — both see each other', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()

    const room = `e2e-duo-${Date.now()}`

    await joinRoom(pageA, room, 'Alice')
    await joinRoom(pageB, room, 'Bob')

    // A sees 2 tiles (own + Bob)
    await expect(pageA.locator('video')).toHaveCount(2, { timeout: 15_000 })
    // B sees 2 tiles (own + Alice)
    await expect(pageB.locator('video')).toHaveCount(2, { timeout: 15_000 })

    await ctxA.close()
    await ctxB.close()
  })

  test('C joins A+B — sees both cameras', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const ctxC = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()
    const pageC = await ctxC.newPage()

    const room = `e2e-trio-${Date.now()}`

    await joinRoom(pageA, room, 'Alice')
    await joinRoom(pageB, room, 'Bob')
    await joinRoom(pageC, room, 'Charlie')

    // C sees 3 tiles (own + Alice + Bob)
    await expect(pageC.locator('video')).toHaveCount(3, { timeout: 15_000 })

    await ctxA.close()
    await ctxB.close()
    await ctxC.close()
  })

  test('B leaves — A sees B disappear', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()

    const room = `e2e-leave-${Date.now()}`

    await joinRoom(pageA, room, 'Alice')
    await joinRoom(pageB, room, 'Bob')

    // A sees 2 tiles
    await expect(pageA.locator('video')).toHaveCount(2, { timeout: 15_000 })

    // B clicks leave
    await pageB.click('button:has(.lucide-phone)')
    await pageB.waitForURL('**/')

    // A should go back to 1 tile
    await expect(pageA.locator('video')).toHaveCount(1, { timeout: 10_000 })

    await ctxA.close()
    await ctxB.close()
  })
})

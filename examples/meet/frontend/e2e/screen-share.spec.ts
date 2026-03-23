import { test, expect } from '@playwright/test'
import { joinRoom } from './helpers/room'

test.describe('Screen share', () => {
  test('A starts screen share — B sees extra tile', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()

    const room = `e2e-screen-${Date.now()}`

    await joinRoom(pageA, room, 'Alice')
    await joinRoom(pageB, room, 'Bob')

    await expect(pageA.locator('video')).toHaveCount(2, { timeout: 15_000 })
    await expect(pageB.locator('video')).toHaveCount(2, { timeout: 15_000 })

    const tilesBefore = await pageB.locator('video').count()

    // A clicks screen share (monitor icon)
    await pageA.click('button:has(.lucide-monitor)')

    // B should see an extra tile (screen share)
    await expect(pageB.locator('video')).toHaveCount(tilesBefore + 1, { timeout: 15_000 })

    await ctxA.close()
    await ctxB.close()
  })

  test('A stops screen share — tile disappears', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()

    const room = `e2e-screen-stop-${Date.now()}`

    await joinRoom(pageA, room, 'Alice')
    await joinRoom(pageB, room, 'Bob')

    await expect(pageA.locator('video')).toHaveCount(2, { timeout: 15_000 })

    // Start screen share
    await pageA.click('button:has(.lucide-monitor)')
    await expect(pageB.locator('video')).toHaveCount(3, { timeout: 15_000 })

    // Stop screen share (button is now active)
    await pageA.click('button:has(.lucide-monitor)')

    // B should go back to 2 tiles
    await expect(pageB.locator('video')).toHaveCount(2, { timeout: 10_000 })

    await ctxA.close()
    await ctxB.close()
  })

  test('Leave stops active screen share', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()

    const room = `e2e-screen-leave-${Date.now()}`

    await joinRoom(pageA, room, 'Alice')
    await joinRoom(pageB, room, 'Bob')

    await expect(pageB.locator('video')).toHaveCount(2, { timeout: 15_000 })

    // A starts screen share
    await pageA.click('button:has(.lucide-monitor)')
    await expect(pageB.locator('video')).toHaveCount(3, { timeout: 15_000 })

    // A leaves
    await pageA.click('button:has(.lucide-phone)')

    // B should see no screen share tile from A (back to just own tile)
    await expect(pageB.locator('video')).toHaveCount(1, { timeout: 10_000 })

    await ctxA.close()
    await ctxB.close()
  })
})

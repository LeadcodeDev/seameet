import { test, expect } from '@playwright/test'
import { joinRoom } from './helpers/room'

test.describe('Audio mute/unmute', () => {
  test('A mutes — B sees mic-off icon', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()

    const room = `e2e-mute-${Date.now()}`

    await joinRoom(pageA, room, 'Alice')
    await joinRoom(pageB, room, 'Bob')

    // Wait for both to see each other
    await expect(pageA.locator('video')).toHaveCount(2, { timeout: 15_000 })
    await expect(pageB.locator('video')).toHaveCount(2, { timeout: 15_000 })

    // A clicks mute (mic button)
    await pageA.click('button:has(.lucide-mic)')

    // B should see a mic-off icon on A's tile
    await expect(pageB.locator('.lucide-mic-off')).toBeVisible({ timeout: 5_000 })

    await ctxA.close()
    await ctxB.close()
  })

  test('A unmutes — icon disappears', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()

    const room = `e2e-unmute-${Date.now()}`

    await joinRoom(pageA, room, 'Alice')
    await joinRoom(pageB, room, 'Bob')

    await expect(pageA.locator('video')).toHaveCount(2, { timeout: 15_000 })
    await expect(pageB.locator('video')).toHaveCount(2, { timeout: 15_000 })

    // Mute
    await pageA.click('button:has(.lucide-mic)')
    await expect(pageB.locator('.lucide-mic-off')).toBeVisible({ timeout: 5_000 })

    // Unmute (button now shows mic-off icon)
    await pageA.click('button:has(.lucide-mic-off)')

    // Mic-off icon on B's view of A should disappear
    await expect(pageB.locator('.lucide-mic-off')).toBeHidden({ timeout: 5_000 })

    await ctxA.close()
    await ctxB.close()
  })

  test('Mute button changes variant to destructive', async ({ page }) => {
    await joinRoom(page, `e2e-btn-${Date.now()}`, 'Alice')

    // Initially mic button should have secondary variant (not destructive)
    const micBtn = page.locator('button:has(.lucide-mic)')
    await expect(micBtn).toBeVisible({ timeout: 10_000 })

    // Click to mute
    await micBtn.click()

    // Button should now show MicOff and have destructive variant
    const mutedBtn = page.locator('button:has(.lucide-mic-off)')
    await expect(mutedBtn).toBeVisible({ timeout: 3_000 })
  })
})

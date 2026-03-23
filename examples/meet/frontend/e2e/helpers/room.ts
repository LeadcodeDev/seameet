import type { Page } from '@playwright/test'

export async function joinRoom(page: Page, roomCode: string, displayName: string) {
  await page.goto('/')
  await page.fill('input[placeholder="Your name"]', displayName)
  await page.fill('input[placeholder="Room code (leave empty to auto-generate)"]', roomCode)
  await page.click('button[type="submit"]')
  await page.waitForURL(`**/room/${roomCode}`)
}

/** Wait for a <video> element to have a real video stream (videoWidth > 0) */
export async function waitForVideoPlaying(page: Page, selector: string, timeout = 15_000) {
  await page.waitForFunction(
    (sel) => {
      const video = document.querySelector(sel) as HTMLVideoElement | null
      return video && video.videoWidth > 0 && video.videoHeight > 0
    },
    selector,
    { timeout }
  )
}

/** Count visible video tiles */
export async function countVideoTiles(page: Page): Promise<number> {
  return page.locator('video').count()
}

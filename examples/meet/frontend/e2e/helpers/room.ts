import type { Page } from '@playwright/test'

export async function joinRoom(page: Page, roomCode: string, displayName: string) {
  await page.goto('/')
  await page.fill('input[placeholder="Your name"]', displayName)
  await page.fill('input[placeholder="Room code (leave empty to auto-generate)"]', roomCode)
  await page.click('button[type="submit"]')
  await page.waitForURL(`**/room/${roomCode}`)
}

/** Join room with camera/mic pre-configured from lobby */
export async function joinRoomWithMedia(
  page: Page,
  roomCode: string,
  displayName: string,
  options: { camera?: boolean; mic?: boolean } = {}
) {
  await page.goto('/')
  await page.fill('input[placeholder="Your name"]', displayName)
  await page.fill('input[placeholder="Room code (leave empty to auto-generate)"]', roomCode)

  // Camera defaults to OFF in lobby — toggle if requested ON
  if (options.camera) {
    await page.click('button:has(.lucide-video-off)')
  }

  // Mic defaults to OFF in lobby — toggle if requested ON
  if (options.mic) {
    await page.click('button:has(.lucide-mic-off)')
  }

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

/** Count visible video tiles (video elements without .hidden class) */
export async function countVideoTiles(page: Page): Promise<number> {
  return page.locator('video:not(.hidden)').count()
}

/** Count tiles showing avatar placeholder */
export async function countPlaceholderTiles(page: Page): Promise<number> {
  return page.locator('span.bg-primary').count()
}

/** Check if a specific participant's tile shows video (not placeholder) */
export async function participantHasVideo(page: Page, name: string): Promise<boolean> {
  // Find the name label, then go to the parent tile container
  const tile = page.locator('div.relative.rounded-lg', {
    has: page.locator(`text="${name}"`)
  })
  const video = tile.locator('video')
  const hasHidden = await video.evaluate((el) => el.classList.contains('hidden'))
  return !hasHidden
}

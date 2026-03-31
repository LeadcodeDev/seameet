import type { Page, Locator } from '@playwright/test'
import { expect } from '@playwright/test'

export async function joinRoom(page: Page, roomCode: string, displayName: string) {
  await page.goto('/')
  await page.fill('[data-testid="input-name"]', displayName)
  await page.fill('[data-testid="input-room-code"]', roomCode)
  await page.click('[data-testid="btn-join"]')
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
  await page.fill('[data-testid="input-name"]', displayName)
  await page.fill('[data-testid="input-room-code"]', roomCode)

  // Camera defaults to OFF in lobby — toggle if requested ON
  if (options.camera) {
    await page.click('[data-testid="lobby-toggle-camera"]')
  }

  // Mic defaults to OFF in lobby — toggle if requested ON
  if (options.mic) {
    await page.click('[data-testid="lobby-toggle-mic"]')
  }

  await page.click('[data-testid="btn-join"]')
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

/** Get the tile locator for a specific participant by name */
export function getTile(page: Page, name: string): Locator {
  return page.locator(`[data-testid="video-tile"][data-participant="${name}"]`)
}

/** Assert that a participant's tile shows video on or off */
export async function expectVideoState(page: Page, name: string, state: 'on' | 'off', timeout = 10_000) {
  const tile = getTile(page, name)
  await expect(tile).toHaveAttribute('data-video', state, { timeout })
}

/** Click the camera toggle button in the room */
export async function toggleCamera(page: Page) {
  await page.click('[data-testid="btn-toggle-camera"]')
}

/** Click the mic toggle button in the room */
export async function toggleMic(page: Page) {
  await page.click('[data-testid="btn-toggle-mic"]')
}

/** Count visible video tiles (tiles with data-video="on") */
export async function countVideoTiles(page: Page): Promise<number> {
  return page.locator('[data-testid="video-tile"][data-video="on"]').count()
}

/** Count tiles showing avatar placeholder */
export async function countPlaceholderTiles(page: Page): Promise<number> {
  return page.locator('[data-testid="avatar-placeholder"]').count()
}

/** Check if a specific participant's tile shows video (not placeholder) */
export async function participantHasVideo(page: Page, name: string): Promise<boolean> {
  const tile = getTile(page, name)
  const videoAttr = await tile.getAttribute('data-video')
  return videoAttr === 'on'
}

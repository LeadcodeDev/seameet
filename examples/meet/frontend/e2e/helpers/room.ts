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

/** Get the camera tile locator for a specific participant (excludes screen share tiles) */
export function getTile(page: Page, name: string): Locator {
  return page
    .locator(`[data-testid="video-tile"][data-participant="${name}"]`)
    .filter({ hasNot: page.locator(`text="${name}'s screen"`) })
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

/**
 * Mock getDisplayMedia so screen sharing works in headless Chromium.
 * Creates a fake video stream using a canvas element.
 * Must be called BEFORE clicking the screen share button.
 */
export async function mockGetDisplayMedia(page: Page) {
  await page.evaluate(() => {
    navigator.mediaDevices.getDisplayMedia = async () => {
      const canvas = Object.assign(document.createElement('canvas'), { width: 640, height: 480 })
      const ctx = canvas.getContext('2d')!
      // Draw a colored rectangle so the stream has actual pixel data
      ctx.fillStyle = '#2563eb'
      ctx.fillRect(0, 0, 640, 480)
      ctx.fillStyle = '#fff'
      ctx.font = '48px sans-serif'
      ctx.fillText('Screen Share', 120, 260)
      return canvas.captureStream(5)
    }
  })
}

/** Click the screen share button in the room */
export async function startScreenShare(page: Page) {
  await mockGetDisplayMedia(page)
  await page.click('[data-testid="btn-screen-share"]')
}

/** Stop screen sharing by clicking the screen share button again */
export async function stopScreenShare(page: Page) {
  await page.click('[data-testid="btn-screen-share"]')
}

/** Get the screen share tile locator for a specific participant */
export function getScreenShareTile(page: Page, name: string): Locator {
  return page.locator(`[data-testid="video-tile"][data-participant="${name}"]`).filter({ hasText: `${name}'s screen` })
}

/** Assert that a screen share tile is visible for a given participant */
export async function expectScreenShareVisible(page: Page, name: string, timeout = 15_000) {
  const tile = getScreenShareTile(page, name)
  await expect(tile).toBeVisible({ timeout })
  await expect(tile).toHaveAttribute('data-video', 'on', { timeout })
}

/** Assert that a screen share tile is NOT visible for a given participant */
export async function expectScreenShareGone(page: Page, name: string, timeout = 10_000) {
  const tile = getScreenShareTile(page, name)
  await expect(tile).toHaveCount(0, { timeout })
}

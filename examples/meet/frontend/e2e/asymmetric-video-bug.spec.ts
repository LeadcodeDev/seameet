import { test, expect } from '@playwright/test'
import { joinRoomWithMedia } from './helpers/room'

/**
 * Reproduces the asymmetric-video bug: User1 joins alone, User2 joins after,
 * and User2 cannot see User1's video (while User1 sees User2's video fine).
 *
 * Verification: instead of relying on tile count, this test inspects the
 * actual <video> element's videoWidth/videoHeight on each participant's
 * remote tile. A real decoded frame produces videoWidth > 0; a black/empty
 * receiver shows videoWidth === 0.
 */
async function getRemoteVideoSize(
  page: import('@playwright/test').Page,
  remoteName: string,
): Promise<{ width: number; height: number; muted: boolean | null }> {
  return page.evaluate((name) => {
    const tile = document.querySelector(
      `[data-testid="video-tile"][data-participant="${name}"]`,
    )
    if (!tile) return { width: 0, height: 0, muted: null }
    const video = tile.querySelector('[data-testid="video-element"]') as HTMLVideoElement | null
    if (!video) return { width: 0, height: 0, muted: null }
    const stream = (video.srcObject as MediaStream | null) ?? null
    const track = stream?.getVideoTracks()[0]
    return {
      width: video.videoWidth,
      height: video.videoHeight,
      muted: track ? track.muted : null,
    }
  }, remoteName)
}

async function expectRemoteVideoPlaying(
  page: import('@playwright/test').Page,
  remoteName: string,
  timeout = 20_000,
) {
  await expect
    .poll(
      async () => {
        const size = await getRemoteVideoSize(page, remoteName)
        return size.width > 0 && size.height > 0
      },
      { timeout, message: `expected ${remoteName}'s remote video to start decoding` },
    )
    .toBe(true)
}

test.describe('Asymmetric video bug', () => {
  test.setTimeout(60_000)
  test('User1 joins alone, then User2 joins — both must see each other', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()

    const room = `e2e-async-${Date.now()}`

    // User1 (Alice) joins alone with camera ON.
    await joinRoomWithMedia(pageA, room, 'Alice', { camera: true, mic: false })
    // Long alone period — long enough that the encoder is past its initial
    // keyframe burst and producing only inter-frames when Bob joins.
    await pageA.waitForTimeout(15_000)

    // User2 (Bob) joins with camera ON.
    await joinRoomWithMedia(pageB, room, 'Bob', { camera: true, mic: false })

    // Both must end up with 2 tiles in their grid.
    await expect(pageA.locator('[data-testid="video-tile"]')).toHaveCount(2, { timeout: 15_000 })
    await expect(pageB.locator('[data-testid="video-tile"]')).toHaveCount(2, { timeout: 15_000 })

    // The bug: Bob's <video> element for Alice never receives decoded frames.
    // We check Bob → Alice (the failing direction) AND Alice → Bob (the passing direction).
    await expectRemoteVideoPlaying(pageA, 'Bob')
    await expectRemoteVideoPlaying(pageB, 'Alice')

    await ctxA.close()
    await ctxB.close()
  })
})

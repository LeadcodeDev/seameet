import { test, expect } from '@playwright/test'
import { joinRoom } from './helpers/room'

test.describe('Video config', () => {
  test('Change resolution via settings sends video_config_changed', async ({ page }) => {
    const wsMessages: string[] = []
    page.on('websocket', ws => {
      ws.on('framesent', frame => {
        if (typeof frame.payload === 'string') wsMessages.push(frame.payload)
      })
    })

    await joinRoom(page, `e2e-res-${Date.now()}`, 'Alice')
    await expect(page.locator('video')).toHaveCount(1, { timeout: 10_000 })

    // Open settings popover
    await page.click('button:has(.lucide-settings)')

    // Select 720p resolution
    await page.selectOption('select >> nth=0', '720p')

    // Check that video_config_changed was sent via WebSocket
    const configMsg = wsMessages
      .map(s => { try { return JSON.parse(s) } catch { return null } })
      .find(m => m?.type === 'video_config_changed')

    expect(configMsg).toBeDefined()
    expect(configMsg.width).toBe(1280)
    expect(configMsg.height).toBe(720)
  })

  test('Change FPS via settings sends video_config_changed', async ({ page }) => {
    const wsMessages: string[] = []
    page.on('websocket', ws => {
      ws.on('framesent', frame => {
        if (typeof frame.payload === 'string') wsMessages.push(frame.payload)
      })
    })

    await joinRoom(page, `e2e-fps-${Date.now()}`, 'Alice')
    await expect(page.locator('video')).toHaveCount(1, { timeout: 10_000 })

    // Open settings popover
    await page.click('button:has(.lucide-settings)')

    // Select 30fps
    await page.selectOption('select >> nth=1', '30')

    const configMsg = wsMessages
      .map(s => { try { return JSON.parse(s) } catch { return null } })
      .find(m => m?.type === 'video_config_changed')

    expect(configMsg).toBeDefined()
    expect(configMsg.fps).toBe(30)
  })
})

import { test, expect, type Page } from '@playwright/test'
import { joinRoom } from './helpers/room'

function captureWsSent(page: Page): string[] {
  const messages: string[] = []
  page.on('websocket', ws => {
    ws.on('framesent', frame => {
      if (typeof frame.payload === 'string') messages.push(frame.payload)
    })
  })
  return messages
}

function captureWsReceived(page: Page): string[] {
  const messages: string[] = []
  page.on('websocket', ws => {
    ws.on('framereceived', frame => {
      if (typeof frame.payload === 'string') messages.push(frame.payload)
    })
  })
  return messages
}

function parseMessages(raw: string[]) {
  return raw.map(s => { try { return JSON.parse(s) } catch { return null } }).filter(Boolean)
}

test.describe('Video config', () => {
  test('Change resolution via settings sends video_config_changed', async ({ page }) => {
    const wsMessages = captureWsSent(page)

    await joinRoom(page, `e2e-res-${Date.now()}`, 'Alice')
    await expect(page.locator('video')).toHaveCount(1, { timeout: 10_000 })

    // Open settings popover
    await page.click('button:has(.lucide-settings)')

    // Select 720p resolution
    await page.selectOption('select >> nth=0', '720p')

    // Check that video_config_changed was sent via WebSocket
    const configMsg = parseMessages(wsMessages)
      .find(m => m?.type === 'video_config_changed')

    expect(configMsg).toBeDefined()
    expect(configMsg.width).toBe(1280)
    expect(configMsg.height).toBe(720)
  })

  test('Change FPS via settings sends video_config_changed', async ({ page }) => {
    const wsMessages = captureWsSent(page)

    await joinRoom(page, `e2e-fps-${Date.now()}`, 'Alice')
    await expect(page.locator('video')).toHaveCount(1, { timeout: 10_000 })

    // Open settings popover
    await page.click('button:has(.lucide-settings)')

    // Select 30fps
    await page.selectOption('select >> nth=1', '30')

    const configMsg = parseMessages(wsMessages)
      .find(m => m?.type === 'video_config_changed')

    expect(configMsg).toBeDefined()
    expect(configMsg.fps).toBe(30)
  })
})

test.describe('Video config multi-participant', () => {
  test('A changes resolution → B receives video_config_changed via WS', async ({ browser }) => {
    const roomCode = `e2e-mp-res-${Date.now()}`

    const aliceCtx = await browser.newContext({ permissions: ['camera', 'microphone'] })
    const bobCtx = await browser.newContext({ permissions: ['camera', 'microphone'] })
    const alice = await aliceCtx.newPage()
    const bob = await bobCtx.newPage()

    const bobReceived = captureWsReceived(bob)

    await joinRoom(alice, roomCode, 'Alice')
    await joinRoom(bob, roomCode, 'Bob')

    // Wait for both to be connected
    await expect(alice.locator('video')).toHaveCount(2, { timeout: 15_000 })
    await expect(bob.locator('video')).toHaveCount(2, { timeout: 15_000 })

    // Alice changes resolution to 720p
    await alice.click('button:has(.lucide-settings)')
    await alice.selectOption('select >> nth=0', '720p')

    // Wait a bit for the message to propagate
    await bob.waitForTimeout(2000)

    const configMsg = parseMessages(bobReceived)
      .find(m => m?.type === 'video_config_changed' && m.width === 1280 && m.height === 720)

    expect(configMsg).toBeDefined()

    await aliceCtx.close()
    await bobCtx.close()
  })

  test('A changes FPS → B receives video_config_changed via WS', async ({ browser }) => {
    const roomCode = `e2e-mp-fps-${Date.now()}`

    const aliceCtx = await browser.newContext({ permissions: ['camera', 'microphone'] })
    const bobCtx = await browser.newContext({ permissions: ['camera', 'microphone'] })
    const alice = await aliceCtx.newPage()
    const bob = await bobCtx.newPage()

    const bobReceived = captureWsReceived(bob)

    await joinRoom(alice, roomCode, 'Alice')
    await joinRoom(bob, roomCode, 'Bob')

    await expect(alice.locator('video')).toHaveCount(2, { timeout: 15_000 })
    await expect(bob.locator('video')).toHaveCount(2, { timeout: 15_000 })

    // Alice changes FPS to 60
    await alice.click('button:has(.lucide-settings)')
    await alice.selectOption('select >> nth=1', '60')

    await bob.waitForTimeout(2000)

    const configMsg = parseMessages(bobReceived)
      .find(m => m?.type === 'video_config_changed' && m.fps === 60)

    expect(configMsg).toBeDefined()

    await aliceCtx.close()
    await bobCtx.close()
  })

  test('3 participants — A changes resolution, B and C receive notification', async ({ browser }) => {
    const roomCode = `e2e-3p-res-${Date.now()}`

    const aliceCtx = await browser.newContext({ permissions: ['camera', 'microphone'] })
    const bobCtx = await browser.newContext({ permissions: ['camera', 'microphone'] })
    const charlieCtx = await browser.newContext({ permissions: ['camera', 'microphone'] })
    const alice = await aliceCtx.newPage()
    const bob = await bobCtx.newPage()
    const charlie = await charlieCtx.newPage()

    const bobReceived = captureWsReceived(bob)
    const charlieReceived = captureWsReceived(charlie)

    await joinRoom(alice, roomCode, 'Alice')
    await joinRoom(bob, roomCode, 'Bob')
    await joinRoom(charlie, roomCode, 'Charlie')

    // Wait for all 3 to see each other
    await expect(alice.locator('video')).toHaveCount(3, { timeout: 15_000 })
    await expect(bob.locator('video')).toHaveCount(3, { timeout: 15_000 })
    await expect(charlie.locator('video')).toHaveCount(3, { timeout: 15_000 })

    // Alice changes to 1080p
    await alice.click('button:has(.lucide-settings)')
    await alice.selectOption('select >> nth=0', '1080p')

    await bob.waitForTimeout(2000)

    const bobMsg = parseMessages(bobReceived)
      .find(m => m?.type === 'video_config_changed' && m.width === 1920 && m.height === 1080)
    const charlieMsg = parseMessages(charlieReceived)
      .find(m => m?.type === 'video_config_changed' && m.width === 1920 && m.height === 1080)

    expect(bobMsg).toBeDefined()
    expect(charlieMsg).toBeDefined()

    await aliceCtx.close()
    await bobCtx.close()
    await charlieCtx.close()
  })

  test('A changes resolution then FPS — B receives 2 distinct messages', async ({ browser }) => {
    const roomCode = `e2e-mp-both-${Date.now()}`

    const aliceCtx = await browser.newContext({ permissions: ['camera', 'microphone'] })
    const bobCtx = await browser.newContext({ permissions: ['camera', 'microphone'] })
    const alice = await aliceCtx.newPage()
    const bob = await bobCtx.newPage()

    const bobReceived = captureWsReceived(bob)

    await joinRoom(alice, roomCode, 'Alice')
    await joinRoom(bob, roomCode, 'Bob')

    await expect(alice.locator('video')).toHaveCount(2, { timeout: 15_000 })
    await expect(bob.locator('video')).toHaveCount(2, { timeout: 15_000 })

    // Alice changes resolution to 720p
    await alice.click('button:has(.lucide-settings)')
    await alice.selectOption('select >> nth=0', '720p')
    await alice.waitForTimeout(500)

    // Alice changes FPS to 30
    await alice.selectOption('select >> nth=1', '30')
    await bob.waitForTimeout(2000)

    const configMsgs = parseMessages(bobReceived)
      .filter(m => m?.type === 'video_config_changed')

    expect(configMsgs.length).toBeGreaterThanOrEqual(2)

    await aliceCtx.close()
    await bobCtx.close()
  })
})

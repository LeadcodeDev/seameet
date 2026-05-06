import { test, expect } from '@playwright/test'
import type { Browser, BrowserContext, Page } from '@playwright/test'
import {
  joinRoomE2EE,
  expectRemoteVideoPlaying,
  expectFullMeshVideo,
  measureTimeToVideo,
  toggleCamera,
  expectVideoState,
  getRemoteVideoSize,
} from './helpers/room'

type Participant = { name: string; ctx: BrowserContext; page: Page }

async function spawnParticipant(browser: Browser, name: string): Promise<Participant> {
  const ctx = await browser.newContext()
  const page = await ctx.newPage()
  return { name, ctx, page }
}

async function closeAll(participants: Participant[]) {
  for (const p of participants) {
    await p.ctx.close()
  }
}

const TTV_BUDGET_MS = 5_000

test.describe('E2EE join handshake', () => {
  test.describe.configure({ mode: 'serial' })

  test('solo: A joins E2EE alone — own video renders', async ({ browser }) => {
    const a = await spawnParticipant(browser, 'Alice')
    const room = `e2ee-solo-${Date.now()}`

    await joinRoomE2EE(a.page, room, a.name)

    await expect(a.page.locator('[data-testid="video-tile"]')).toHaveCount(1, { timeout: 10_000 })
    const size = await getRemoteVideoSize(a.page, a.name)
    expect(size.width).toBeGreaterThan(0)
    expect(size.height).toBeGreaterThan(0)

    await closeAll([a])
  })

  test('duo simultaneous: A+B both E2EE — each sees the other decoded', async ({ browser }) => {
    const a = await spawnParticipant(browser, 'Alice')
    const b = await spawnParticipant(browser, 'Bob')
    const room = `e2ee-duo-sim-${Date.now()}`

    await Promise.all([joinRoomE2EE(a.page, room, a.name), joinRoomE2EE(b.page, room, b.name)])

    await expect(a.page.locator('[data-testid="video-tile"]')).toHaveCount(2, { timeout: 15_000 })
    await expect(b.page.locator('[data-testid="video-tile"]')).toHaveCount(2, { timeout: 15_000 })

    await expectRemoteVideoPlaying(a.page, b.name)
    await expectRemoteVideoPlaying(b.page, a.name)

    await closeAll([a, b])
  })

  test('duo late join: A alone, B joins later — both see each other under E2EE', async ({ browser }) => {
    test.setTimeout(60_000)
    const a = await spawnParticipant(browser, 'Alice')
    const b = await spawnParticipant(browser, 'Bob')
    const room = `e2ee-duo-late-${Date.now()}`

    await joinRoomE2EE(a.page, room, a.name)
    // Long enough that the encoder is past its initial keyframe burst.
    await a.page.waitForTimeout(8_000)

    await joinRoomE2EE(b.page, room, b.name)

    await expect(a.page.locator('[data-testid="video-tile"]')).toHaveCount(2, { timeout: 15_000 })
    await expect(b.page.locator('[data-testid="video-tile"]')).toHaveCount(2, { timeout: 15_000 })

    await expectRemoteVideoPlaying(a.page, b.name)
    await expectRemoteVideoPlaying(b.page, a.name)

    await closeAll([a, b])
  })

  test('duo late join (very late): A waits 15s alone, then B joins — symmetric video', async ({ browser }) => {
    test.setTimeout(75_000)
    const a = await spawnParticipant(browser, 'Alice')
    const b = await spawnParticipant(browser, 'Bob')
    const room = `e2ee-duo-verylate-${Date.now()}`

    await joinRoomE2EE(a.page, room, a.name)
    await a.page.waitForTimeout(15_000)

    await joinRoomE2EE(b.page, room, b.name)

    await expectRemoteVideoPlaying(a.page, b.name, 25_000)
    await expectRemoteVideoPlaying(b.page, a.name, 25_000)

    await closeAll([a, b])
  })

  test('trio simultaneous: A+B+C all E2EE — full mesh video', async ({ browser }) => {
    test.setTimeout(60_000)
    const a = await spawnParticipant(browser, 'Alice')
    const b = await spawnParticipant(browser, 'Bob')
    const c = await spawnParticipant(browser, 'Charlie')
    const room = `e2ee-trio-sim-${Date.now()}`

    await Promise.all([
      joinRoomE2EE(a.page, room, a.name),
      joinRoomE2EE(b.page, room, b.name),
      joinRoomE2EE(c.page, room, c.name),
    ])

    for (const p of [a, b, c]) {
      await expect(p.page.locator('[data-testid="video-tile"]')).toHaveCount(3, { timeout: 20_000 })
    }

    await expectFullMeshVideo([a, b, c])

    await closeAll([a, b, c])
  })

  test('trio cascading: A → wait → B → wait → C — every cross-pair has video', async ({ browser }) => {
    test.setTimeout(75_000)
    const a = await spawnParticipant(browser, 'Alice')
    const b = await spawnParticipant(browser, 'Bob')
    const c = await spawnParticipant(browser, 'Charlie')
    const room = `e2ee-trio-cascade-${Date.now()}`

    await joinRoomE2EE(a.page, room, a.name)
    await a.page.waitForTimeout(5_000)

    await joinRoomE2EE(b.page, room, b.name)
    // Stabilise the A↔B handshake before C joins.
    await expectRemoteVideoPlaying(a.page, b.name)
    await expectRemoteVideoPlaying(b.page, a.name)
    await b.page.waitForTimeout(5_000)

    await joinRoomE2EE(c.page, room, c.name)

    await expectFullMeshVideo([a, b, c], 25_000)

    await closeAll([a, b, c])
  })

  test('trio late single: A+B already in room, C joins late — C sees both, both see C', async ({ browser }) => {
    test.setTimeout(75_000)
    const a = await spawnParticipant(browser, 'Alice')
    const b = await spawnParticipant(browser, 'Bob')
    const c = await spawnParticipant(browser, 'Charlie')
    const room = `e2ee-trio-late-${Date.now()}`

    await Promise.all([joinRoomE2EE(a.page, room, a.name), joinRoomE2EE(b.page, room, b.name)])
    await expectRemoteVideoPlaying(a.page, b.name)
    await expectRemoteVideoPlaying(b.page, a.name)

    await a.page.waitForTimeout(8_000)

    await joinRoomE2EE(c.page, room, c.name)

    // C must see both existing peers.
    await expectRemoteVideoPlaying(c.page, a.name)
    await expectRemoteVideoPlaying(c.page, b.name)
    // Both existing peers must see C.
    await expectRemoteVideoPlaying(a.page, c.name)
    await expectRemoteVideoPlaying(b.page, c.name)

    await closeAll([a, b, c])
  })

  test('quad E2EE: 4 participants joining sequentially form a full mesh', async ({ browser }) => {
    test.setTimeout(90_000)
    const a = await spawnParticipant(browser, 'Alice')
    const b = await spawnParticipant(browser, 'Bob')
    const c = await spawnParticipant(browser, 'Charlie')
    const d = await spawnParticipant(browser, 'Dora')
    const room = `e2ee-quad-${Date.now()}`

    await joinRoomE2EE(a.page, room, a.name)
    await joinRoomE2EE(b.page, room, b.name)
    await joinRoomE2EE(c.page, room, c.name)
    await joinRoomE2EE(d.page, room, d.name)

    for (const p of [a, b, c, d]) {
      await expect(p.page.locator('[data-testid="video-tile"]')).toHaveCount(4, { timeout: 25_000 })
    }

    await expectFullMeshVideo([a, b, c, d], 30_000)

    await closeAll([a, b, c, d])
  })

  test('quint E2EE: 5 participants — every cross-pair decodes', async ({ browser }) => {
    test.setTimeout(120_000)
    const participants = await Promise.all(
      ['Alice', 'Bob', 'Charlie', 'Dora', 'Eve'].map((n) => spawnParticipant(browser, n))
    )
    const room = `e2ee-quint-${Date.now()}`

    for (const p of participants) {
      await joinRoomE2EE(p.page, room, p.name)
    }

    for (const p of participants) {
      await expect(p.page.locator('[data-testid="video-tile"]')).toHaveCount(5, { timeout: 30_000 })
    }

    await expectFullMeshVideo(participants, 35_000)

    await closeAll(participants)
  })

  test('camera-off-then-on under E2EE: B joins with camera OFF, toggles ON later — A sees B video', async ({
    browser,
  }) => {
    test.setTimeout(60_000)
    const a = await spawnParticipant(browser, 'Alice')
    const b = await spawnParticipant(browser, 'Bob')
    const room = `e2ee-cam-toggle-${Date.now()}`

    await joinRoomE2EE(a.page, room, a.name, { camera: true })
    await joinRoomE2EE(b.page, room, b.name, { camera: false })

    // While B's camera is off, A sees a placeholder for B.
    await expectVideoState(a.page, b.name, 'off')

    // B turns the camera on; A must receive a decryptable, decodable stream.
    await toggleCamera(b.page)
    await expectVideoState(a.page, b.name, 'on', 15_000)
    await expectRemoteVideoPlaying(a.page, b.name)

    // Reverse direction: B should also see A's video the whole time.
    await expectRemoteVideoPlaying(b.page, a.name)

    await closeAll([a, b])
  })

  test('leave-and-rejoin under E2EE: B leaves, then rejoins — A sees B again', async ({ browser }) => {
    test.setTimeout(75_000)
    const a = await spawnParticipant(browser, 'Alice')
    const b = await spawnParticipant(browser, 'Bob')
    const room = `e2ee-rejoin-${Date.now()}`

    await joinRoomE2EE(a.page, room, a.name)
    await joinRoomE2EE(b.page, room, b.name)
    await expectRemoteVideoPlaying(a.page, b.name)
    await expectRemoteVideoPlaying(b.page, a.name)

    await b.page.click('[data-testid="btn-leave"]')
    await b.page.waitForURL('**/')
    await expect(a.page.locator('[data-testid="video-tile"]')).toHaveCount(1, { timeout: 10_000 })

    // B rejoins the same room.
    await joinRoomE2EE(b.page, room, b.name)
    await expect(a.page.locator('[data-testid="video-tile"]')).toHaveCount(2, { timeout: 15_000 })

    await expectRemoteVideoPlaying(a.page, b.name)
    await expectRemoteVideoPlaying(b.page, a.name)

    await closeAll([a, b])
  })

  test('TTV budget: late joiner gets first decoded frame within budget for each existing peer', async ({
    browser,
  }) => {
    test.setTimeout(75_000)
    const a = await spawnParticipant(browser, 'Alice')
    const b = await spawnParticipant(browser, 'Bob')
    const c = await spawnParticipant(browser, 'Charlie')
    const room = `e2ee-ttv-${Date.now()}`

    await joinRoomE2EE(a.page, room, a.name)
    await joinRoomE2EE(b.page, room, b.name)
    await expectRemoteVideoPlaying(a.page, b.name)
    await expectRemoteVideoPlaying(b.page, a.name)
    await a.page.waitForTimeout(3_000)

    // Start C's join and immediately measure how long until each existing peer
    // is visible. measureTimeToVideo polls from "now" until videoWidth > 0.
    await joinRoomE2EE(c.page, room, c.name)
    const ttvA = await measureTimeToVideo(c.page, a.name)
    const ttvB = await measureTimeToVideo(c.page, b.name)

    // The strict spec target is 1s p99.9 — under fake-device + local SFU this
    // should be well below TTV_BUDGET_MS. If this fails, the join handshake
    // sequencing or keyframe coordination has regressed.
    expect(ttvA, `Alice TTV from C's perspective`).toBeLessThan(TTV_BUDGET_MS)
    expect(ttvB, `Bob TTV from C's perspective`).toBeLessThan(TTV_BUDGET_MS)

    await closeAll([a, b, c])
  })

  test('mixed mode: A E2EE-on, B E2EE-off — at minimum each sees its own video', async ({ browser }) => {
    test.setTimeout(45_000)
    const a = await spawnParticipant(browser, 'Alice')
    const b = await spawnParticipant(browser, 'Bob')
    const room = `e2ee-mixed-${Date.now()}`

    await joinRoomE2EE(a.page, room, a.name)
    // B joins WITHOUT enabling E2EE.
    await a.page.waitForTimeout(1_000)
    await b.page.goto('/')
    await b.page.fill('[data-testid="input-name"]', b.name)
    await b.page.fill('[data-testid="input-room-code"]', room)
    await b.page.click('[data-testid="lobby-toggle-camera"]')
    await b.page.click('[data-testid="btn-join"]')
    await b.page.waitForURL(`**/room/${room}`)

    // Both pages should display two tiles (signaling-level connectivity).
    await expect(a.page.locator('[data-testid="video-tile"]')).toHaveCount(2, { timeout: 15_000 })
    await expect(b.page.locator('[data-testid="video-tile"]')).toHaveCount(2, { timeout: 15_000 })

    // Each side at minimum sees its own local video.
    await expectRemoteVideoPlaying(a.page, a.name)
    await expectRemoteVideoPlaying(b.page, b.name)

    // Cross-decode behaviour is not yet specified for mixed-E2EE rooms; we record
    // it here for diagnostic visibility but do not assert success.
    const aSeesB = await getRemoteVideoSize(a.page, b.name)
    const bSeesA = await getRemoteVideoSize(b.page, a.name)
    test.info().annotations.push({
      type: 'mixed-e2ee-cross-decode',
      description: `A→B width=${aSeesB.width}, B→A width=${bSeesA.width}`,
    })

    await closeAll([a, b])
  })

  test('symmetry under E2EE: every ordered pair (viewer, subject) decodes for trio', async ({ browser }) => {
    test.setTimeout(75_000)
    const a = await spawnParticipant(browser, 'Alice')
    const b = await spawnParticipant(browser, 'Bob')
    const c = await spawnParticipant(browser, 'Charlie')
    const room = `e2ee-sym-${Date.now()}`

    await Promise.all([
      joinRoomE2EE(a.page, room, a.name),
      joinRoomE2EE(b.page, room, b.name),
      joinRoomE2EE(c.page, room, c.name),
    ])

    const pairs: Array<[Participant, Participant]> = [
      [a, b],
      [b, a],
      [a, c],
      [c, a],
      [b, c],
      [c, b],
    ]

    for (const [viewer, subject] of pairs) {
      await expectRemoteVideoPlaying(viewer.page, subject.name, 25_000)
    }

    await closeAll([a, b, c])
  })
})

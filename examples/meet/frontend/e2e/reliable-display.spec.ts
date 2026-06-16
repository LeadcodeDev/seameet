import { test, expect } from '@playwright/test'
import { joinRoomWithMedia, expectVideoState, toggleCamera, getTile } from './helpers/room'

test.describe('Reliable participant display', () => {
  test('peer leaving removes its tile for everyone (INV-1, no ghost tile)', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()
    const room = `e2e-ghost-${Date.now()}`

    await joinRoomWithMedia(pageA, room, 'Alice', { camera: true, mic: true })
    await joinRoomWithMedia(pageB, room, 'Bob', { camera: true, mic: true })
    await expect(getTile(pageA, 'Bob')).toHaveCount(1)

    await ctxB.close() // graceful leave → WS close → room_status without Bob

    await expect(getTile(pageA, 'Bob')).toHaveCount(0, { timeout: 10_000 })
    await ctxA.close()
  })

  test('toggling one camera does not change another participant tile (INV-4)', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const ctxC = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()
    const pageC = await ctxC.newPage()
    const room = `e2e-isolation-${Date.now()}`

    await joinRoomWithMedia(pageA, room, 'Alice', { camera: true, mic: true })
    await joinRoomWithMedia(pageB, room, 'Bob', { camera: true, mic: true })
    await joinRoomWithMedia(pageC, room, 'Charlie', { camera: false, mic: true })

    await expectVideoState(pageA, 'Bob', 'on')
    await expectVideoState(pageA, 'Charlie', 'off')

    await toggleCamera(pageB)

    await expectVideoState(pageA, 'Bob', 'off')
    await expectVideoState(pageA, 'Charlie', 'off')
    await expectVideoState(pageC, 'Charlie', 'off')

    await ctxA.close(); await ctxB.close(); await ctxC.close()
  })

  test('late joiner sees correct state for every existing participant (INV-3)', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const ctxC = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()
    const pageC = await ctxC.newPage()
    const room = `e2e-late-${Date.now()}`

    await joinRoomWithMedia(pageA, room, 'Alice', { camera: true, mic: true })
    await joinRoomWithMedia(pageB, room, 'Bob', { camera: false, mic: true })

    await joinRoomWithMedia(pageC, room, 'Charlie', { camera: true, mic: true })
    await expectVideoState(pageC, 'Alice', 'on')
    await expectVideoState(pageC, 'Bob', 'off')

    await ctxA.close(); await ctxB.close(); await ctxC.close()
  })
})

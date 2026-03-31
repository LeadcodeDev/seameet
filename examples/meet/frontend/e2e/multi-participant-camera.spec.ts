import { test, expect } from '@playwright/test'
import { joinRoomWithMedia, expectVideoState, toggleCamera, getTile } from './helpers/room'

test.describe('Multi-participant camera ON/OFF (7 scenarios)', () => {
  test('Sequential camera toggle flow with 3 participants', async ({ browser }) => {
    const ctxA = await browser.newContext()
    const ctxB = await browser.newContext()
    const ctxC = await browser.newContext()
    const pageA = await ctxA.newPage()
    const pageB = await ctxB.newPage()
    const pageC = await ctxC.newPage()

    const room = `e2e-multi-cam-${Date.now()}`

    // --- Scenario 1: A joins camera OFF → A sees own placeholder ---
    await joinRoomWithMedia(pageA, room, 'Alice', { camera: false, mic: true })
    await expectVideoState(pageA, 'Alice', 'off')

    // --- Scenario 2: B joins camera OFF → B sees 2 placeholders, A sees B placeholder ---
    await joinRoomWithMedia(pageB, room, 'Bob', { camera: false, mic: true })

    await expectVideoState(pageB, 'Bob', 'off')
    await expectVideoState(pageB, 'Alice', 'off')
    await expectVideoState(pageA, 'Bob', 'off')

    // --- Scenario 3: A activates camera → A sees own video, B sees A video + own placeholder ---
    await toggleCamera(pageA)

    await expectVideoState(pageA, 'Alice', 'on')
    await expectVideoState(pageB, 'Alice', 'on')
    await expectVideoState(pageB, 'Bob', 'off')

    // --- Scenario 4: C joins camera ON → C sees own video + A video + B placeholder ---
    await joinRoomWithMedia(pageC, room, 'Charlie', { camera: true, mic: true })

    await expectVideoState(pageC, 'Charlie', 'on')
    await expectVideoState(pageC, 'Alice', 'on')
    await expectVideoState(pageC, 'Bob', 'off')

    // A and B see C's video
    await expectVideoState(pageA, 'Charlie', 'on')
    await expectVideoState(pageB, 'Charlie', 'on')

    // --- Scenario 5: A turns camera OFF → B and C see A placeholder ---
    await toggleCamera(pageA)

    await expectVideoState(pageA, 'Alice', 'off')
    await expectVideoState(pageB, 'Alice', 'off')
    await expectVideoState(pageC, 'Alice', 'off')

    // B still off, C still on
    await expectVideoState(pageB, 'Bob', 'off')
    await expectVideoState(pageC, 'Charlie', 'on')

    // --- Scenario 6: C turns camera OFF → A and B see C placeholder ---
    await toggleCamera(pageC)

    await expectVideoState(pageC, 'Charlie', 'off')
    await expectVideoState(pageA, 'Charlie', 'off')
    await expectVideoState(pageB, 'Charlie', 'off')

    // All should now be off
    await expectVideoState(pageA, 'Alice', 'off')
    await expectVideoState(pageB, 'Bob', 'off')

    // --- Scenario 7: A re-enables camera → B and C see A video ---
    await toggleCamera(pageA)

    await expectVideoState(pageA, 'Alice', 'on')
    await expectVideoState(pageB, 'Alice', 'on')
    await expectVideoState(pageC, 'Alice', 'on')

    // B and C still off
    await expectVideoState(pageB, 'Bob', 'off')
    await expectVideoState(pageC, 'Charlie', 'off')

    await ctxA.close()
    await ctxB.close()
    await ctxC.close()
  })
})

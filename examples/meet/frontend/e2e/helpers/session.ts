import type { Page } from '@playwright/test'

/**
 * Intercepts `POST /rooms/:room_id/participants` for the given page and
 * fulfills it locally. Generates a fresh ParticipantId per call. The
 * example backend no longer issues a session token — the bearer the
 * frontend already holds (from the `getAuthToken()` stub) is reused as
 * the WS Join token directly.
 *
 * Must be called BEFORE `page.goto(...)` so the route is in place when
 * the frontend issues its POST.
 */
export async function mockRoomSession(page: Page): Promise<void> {
  await page.route('**/rooms/*/participants', async (route, request) => {
    if (request.method() !== 'POST') {
      await route.fallback()
      return
    }
    const url = new URL(request.url())
    const segments = url.pathname.split('/').filter(Boolean)
    const roomIdx = segments.indexOf('rooms')
    const roomId =
      roomIdx >= 0 && segments.length > roomIdx + 1
        ? decodeURIComponent(segments[roomIdx + 1])
        : ''
    const body = (() => {
      try {
        return request.postDataJSON() as { display_name?: string | null }
      } catch {
        return { display_name: null }
      }
    })()
    const participantId = crypto.randomUUID()
    await route.fulfill({
      status: 201,
      contentType: 'application/json',
      body: JSON.stringify({
        room: { id: roomId },
        participant: { id: participantId, display_name: body.display_name ?? null },
      }),
    })
  })
}

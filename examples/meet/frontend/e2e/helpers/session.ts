import { createHmac } from 'node:crypto'
import type { Page } from '@playwright/test'

/**
 * Deterministic 32-byte secret shared between Playwright (mints JWTs) and
 * the Rust backend (verifies JWTs). Set in playwright.config.ts via the
 * `SEAMEET_SESSION_SECRET` env var on the cargo `webServer` entry.
 */
export const SESSION_SECRET = 'seameet-playwright-secret-32-bytes!'

function b64url(input: Buffer | string): string {
  return Buffer.from(input).toString('base64url')
}

/**
 * Mints a HS256 JWT matching the SessionClaims struct that the Rust
 * `session.rs::consume` decodes. Same secret + same claim shape as the
 * production `create_session` so the backend accepts it identically.
 */
export function mintSessionToken(
  participantId: string,
  roomId: string,
  ttlSeconds = 300,
): string {
  const header = { typ: 'JWT', alg: 'HS256' }
  const now = Math.floor(Date.now() / 1000)
  const claims = {
    pid: participantId,
    room_id: roomId,
    iat: now,
    exp: now + ttlSeconds,
  }
  const data = `${b64url(JSON.stringify(header))}.${b64url(JSON.stringify(claims))}`
  const sig = createHmac('sha256', SESSION_SECRET).update(data).digest('base64url')
  return `${data}.${sig}`
}

/**
 * Intercepts `POST /rooms/:room_id/participants` for the given page and
 * fulfills it locally. Generates a fresh ParticipantId per call and
 * mints a matching session JWT. The Rust SFU still validates the JWT —
 * only the HTTP roundtrip is mocked.
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
    const token = mintSessionToken(participantId, roomId)
    await route.fulfill({
      status: 201,
      contentType: 'application/json',
      body: JSON.stringify({
        room: { id: roomId },
        participant: { id: participantId, display_name: body.display_name ?? null },
        session: { token },
      }),
    })
  })
}

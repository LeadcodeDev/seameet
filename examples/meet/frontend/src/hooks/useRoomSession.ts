import { useEffect, useRef, useState } from 'react'

/**
 * Server-issued participant identity created by
 * `POST /rooms/:room_id/participants`. The `participantId` is authoritative
 * — the client never invents it. The bearer token used for the HTTP call
 * is also the one the WebSocket layer presents to the SFU's
 * `on_authenticate` hook (see `useSignaling.join`).
 */
export interface RoomSession {
  participantId: string
}

export type RoomSessionStatus = 'idle' | 'creating' | 'ready' | 'error'

export interface UseRoomSessionResult {
  session: RoomSession | null
  status: RoomSessionStatus
  error: string | null
  retry: () => void
}

const DEFAULT_HTTP_URL =
  import.meta.env.VITE_HTTP_URL ?? `http://${window.location.hostname}:3002`

function cacheKey(roomId: string, displayName: string): string {
  return `seameet-session:${roomId}:${displayName}`
}

function readCached(roomId: string, displayName: string): RoomSession | null {
  try {
    const raw = sessionStorage.getItem(cacheKey(roomId, displayName))
    if (!raw) return null
    const parsed = JSON.parse(raw) as RoomSession
    if (typeof parsed.participantId !== 'string') return null
    return { participantId: parsed.participantId }
  } catch {
    return null
  }
}

function writeCached(roomId: string, displayName: string, session: RoomSession): void {
  try {
    sessionStorage.setItem(cacheKey(roomId, displayName), JSON.stringify(session))
  } catch {
    // Quota / disabled storage — caller still gets the session in-memory.
  }
}

interface CreateParticipantResponse {
  room: { id: string }
  participant: { id: string; display_name?: string | null }
}

/**
 * Creates a server-side participant for `roomId` via REST and returns the
 * resulting identity. Caches the participant id in sessionStorage keyed by
 * (roomId, displayName) so a StrictMode remount (or a manual page refresh
 * within the tab) reuses the same identity instead of minting a new one.
 *
 * The hook does NOT open the WebSocket — that's still `useSignaling`'s
 * job. Consumers should gate `useSignaling` (and the broader CallProvider)
 * on `status === 'ready'`.
 *
 * `authToken` is the bearer credential forwarded to the backend. Pass
 * whatever your IAM returns; in this example it is a stub from
 * `lib/auth.ts::getAuthToken()`.
 */
export function useRoomSession(
  roomId: string | undefined,
  displayName: string | undefined,
  options: { authToken?: string; httpUrl?: string } = {},
): UseRoomSessionResult {
  const [session, setSession] = useState<RoomSession | null>(() => {
    if (!roomId || !displayName) return null
    return readCached(roomId, displayName)
  })
  const [status, setStatus] = useState<RoomSessionStatus>(() => {
    if (!roomId || !displayName) return 'idle'
    return readCached(roomId, displayName) ? 'ready' : 'creating'
  })
  const [error, setError] = useState<string | null>(null)
  const [attempt, setAttempt] = useState(0)

  // Guard against StrictMode double-effect: dedupe in-flight POSTs by
  // keying a ref on (roomId, displayName, attempt). The second mount
  // sees the same key, finds an in-flight promise, and waits for it.
  const inFlightRef = useRef<Map<string, Promise<RoomSession>>>(new Map())

  useEffect(() => {
    if (!roomId || !displayName) {
      setStatus('idle')
      return
    }

    const cached = readCached(roomId, displayName)
    if (cached) {
      setSession(cached)
      setStatus('ready')
      return
    }

    setStatus('creating')
    setError(null)
    const httpBase = options.httpUrl ?? DEFAULT_HTTP_URL
    const url = `${httpBase}/rooms/${encodeURIComponent(roomId)}/participants`
    const dedupKey = `${roomId}|${displayName}|${attempt}`
    let cancelled = false

    let promise = inFlightRef.current.get(dedupKey)
    if (!promise) {
      promise = (async () => {
        const headers: Record<string, string> = { 'Content-Type': 'application/json' }
        if (options.authToken) {
          headers.Authorization = `Bearer ${options.authToken}`
        }
        const resp = await fetch(url, {
          method: 'POST',
          headers,
          body: JSON.stringify({ display_name: displayName }),
        })
        if (!resp.ok) {
          let message = `HTTP ${resp.status}`
          try {
            const body = (await resp.json()) as { message?: string }
            if (body?.message) message = body.message
          } catch { /* ignore */ }
          throw new Error(message)
        }
        const data = (await resp.json()) as CreateParticipantResponse
        const next: RoomSession = { participantId: data.participant.id }
        writeCached(roomId, displayName, next)
        return next
      })()
      inFlightRef.current.set(dedupKey, promise)
    }

    promise
      .then((next) => {
        if (cancelled) return
        setSession(next)
        setStatus('ready')
      })
      .catch((err: unknown) => {
        if (cancelled) return
        const message = err instanceof Error ? err.message : 'unknown error'
        setError(message)
        setStatus('error')
      })
      .finally(() => {
        inFlightRef.current.delete(dedupKey)
      })

    return () => {
      cancelled = true
    }
    // We intentionally exclude options.authToken / options.httpUrl from
    // deps — changing those mid-room would just keep the existing session.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [roomId, displayName, attempt])

  return {
    session,
    status,
    error,
    retry: () => setAttempt((n) => n + 1),
  }
}

export type RoomMode = 'meeting' | 'large' | 'webinar'

export const MEETING_MAX = 30
export const WEBINAR_MIN = 50

/**
 * In `large` mode we keep at most this many tiles with live video — the
 * rest fall back to avatar-only. Picked to match the visible budget the
 * `meeting` mode handles comfortably without overwhelming the decoder.
 */
export const LARGE_VIDEO_BUDGET = 12

/**
 * Classify a room based on total tile count (local + remote + screen
 * shares — the same count the layout already uses to switch grids).
 *
 *   ≤ MEETING_MAX  → 'meeting'  (current behaviour, all tiles get video)
 *   ≤ WEBINAR_MIN  → 'large'    (cap video tiles, rest are avatars)
 *   else           → 'webinar'  (only the active speaker shows video)
 */
export function classifyRoom(totalTiles: number): RoomMode {
  if (totalTiles <= MEETING_MAX) return 'meeting'
  if (totalTiles <= WEBINAR_MIN) return 'large'
  return 'webinar'
}

export interface VisibleVideoSetInput {
  /** Every peer id that could potentially be rendered, in deterministic order. */
  peerIds: string[]
  /** The local participant id — always allowed to see their own video. */
  localId: string
  /** Currently-talking participant, if any. */
  activeSpeakerId: string | null
  /** Most-recent-first speaker history (capped by the caller). */
  recentSpeakers: string[]
  mode: RoomMode
}

/**
 * Decide which peers should render with their video stream attached.
 * Peers not in the returned set should render an avatar-only tile.
 *
 * Local always sees their own video (so it's *not* in this set, but the
 * caller is expected to render the local tile unconditionally).
 *
 * Order of preference for filling the budget in `large` mode:
 *   1. The active speaker (if any).
 *   2. Recent speakers, most-recent first.
 *   3. The remaining peers in iteration order (deterministic).
 */
export function pickVisibleVideoSet(input: VisibleVideoSetInput): Set<string> {
  const { peerIds, localId, activeSpeakerId, recentSpeakers, mode } = input

  if (mode === 'meeting') {
    return new Set(peerIds)
  }

  const visible = new Set<string>()

  const consider = (id: string | null | undefined) => {
    if (!id) return
    if (id === localId) return
    if (!peerIds.includes(id)) return
    visible.add(id)
  }

  consider(activeSpeakerId)

  if (mode === 'webinar') {
    return visible
  }

  // large: fill remaining slots with recent speakers, then the rest.
  for (const speaker of recentSpeakers) {
    if (visible.size >= LARGE_VIDEO_BUDGET) break
    consider(speaker)
  }
  for (const peer of peerIds) {
    if (visible.size >= LARGE_VIDEO_BUDGET) break
    visible.add(peer)
  }

  return visible
}

/**
 * Push a new speaker onto a recent-speakers list while preserving the
 * "most recent first, no duplicates, capped" invariant. Returns a new
 * array — the input is never mutated.
 */
export function pushRecentSpeaker(
  current: string[],
  speaker: string,
  cap = 5,
): string[] {
  const next = [speaker, ...current.filter(id => id !== speaker)]
  if (next.length > cap) next.length = cap
  return next
}

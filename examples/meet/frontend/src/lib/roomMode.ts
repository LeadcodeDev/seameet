/**
 * The maximum number of peer tiles that should show live video simultaneously.
 * When the total tile count (local + peers + screen shares) stays within this
 * budget, every peer gets video. When it exceeds it, only the most-active
 * speakers receive a live stream; the rest render an avatar placeholder inside
 * the same grid.
 */
export const VIDEO_GRID_BUDGET = 10

export interface VisibleVideoSetInput {
  /** Every peer id that could potentially be rendered, in deterministic order. */
  peerIds: string[]
  /** The local participant id — always renders their own video unconditionally. */
  localId: string
  /** Currently-talking participant, if any. */
  activeSpeakerId: string | null
  /** Most-recent-first speaker history (capped by the caller). */
  recentSpeakers: string[]
  /** Maximum number of peers to show video for. Defaults to VIDEO_GRID_BUDGET. */
  budget?: number
}

/**
 * Decide which peers should render with their video stream attached.
 * Peers not in the returned set should render an avatar-only placeholder (same
 * VideoTile component, videoEnabled=false) inside the uniform grid.
 *
 * Local always sees their own video (so it is *not* in this set — the caller
 * renders the local tile unconditionally).
 *
 * Fill order (up to `budget`):
 *   1. The active speaker (if any).
 *   2. Recent speakers, most-recent first.
 *   3. The remaining peers in iteration order (deterministic).
 */
export function pickVisibleVideoSet(input: VisibleVideoSetInput): Set<string> {
  const { peerIds, localId, activeSpeakerId, recentSpeakers, budget = VIDEO_GRID_BUDGET } = input

  const visible = new Set<string>()

  const consider = (id: string | null | undefined) => {
    if (!id) return
    if (id === localId) return
    if (!peerIds.includes(id)) return
    if (visible.size >= budget) return
    visible.add(id)
  }

  consider(activeSpeakerId)

  for (const speaker of recentSpeakers) {
    if (visible.size >= budget) break
    consider(speaker)
  }

  for (const peer of peerIds) {
    if (visible.size >= budget) break
    consider(peer)
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

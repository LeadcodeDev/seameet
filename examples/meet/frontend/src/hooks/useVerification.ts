import { useCallback, useEffect, useRef, useState } from 'react'

export type VerificationStatus = 'unverified' | 'verified' | 'changed'

export interface UseVerification {
  /** Status per peer id. Peers not in the map are 'unverified' by default. */
  statuses: Map<string, VerificationStatus>
  /** Mark a peer as verified, snapshotting its current safety number. */
  markVerified: (peerId: string) => void
  /** Clear the verification flag for a peer. */
  clear: (peerId: string) => void
  /** Quick check used by callers — equivalent to statuses.get(peerId). */
  status: (peerId: string) => VerificationStatus
}

/**
 * Per-session "I've confirmed this safety number matches" state. The user
 * verifies a peer once during a call; if that peer's safety number ever
 * changes afterwards (e.g. they reconnect with fresh keys, or someone is
 * trying to MITM mid-call), the verified flag flips to `changed` so the
 * UI can flag it. Verification deliberately does NOT survive page reload —
 * rooms are ephemeral and persisting trust would be a footgun.
 *
 * Inputs:
 *   - `safetyNumbers`: peerId → current safety number, as exposed by
 *     `useE2EE`. The hook watches this map and resets verification when a
 *     peer's number diverges from what was snapshot at verify time.
 */
export function useVerification(safetyNumbers: Map<string, string>): UseVerification {
  const [statuses, setStatuses] = useState<Map<string, VerificationStatus>>(new Map())
  // Snapshot of the safety number that was on display when the user
  // pressed "Mark as verified". We compare against this on every change.
  const verifiedAtRef = useRef<Map<string, string>>(new Map())

  useEffect(() => {
    setStatuses(prev => {
      let mutated = false
      const next = new Map(prev)
      for (const [peerId, snapshot] of verifiedAtRef.current.entries()) {
        const current = safetyNumbers.get(peerId)
        if (current === undefined) {
          // Peer left or hasn't completed key exchange — drop the snapshot
          // so a future re-verify works cleanly.
          verifiedAtRef.current.delete(peerId)
          if (next.has(peerId)) {
            next.delete(peerId)
            mutated = true
          }
          continue
        }
        if (current !== snapshot && next.get(peerId) !== 'changed') {
          next.set(peerId, 'changed')
          mutated = true
        }
      }
      return mutated ? next : prev
    })
  }, [safetyNumbers])

  const markVerified = useCallback((peerId: string) => {
    const current = safetyNumbers.get(peerId)
    if (!current) return
    verifiedAtRef.current.set(peerId, current)
    setStatuses(prev => {
      const next = new Map(prev)
      next.set(peerId, 'verified')
      return next
    })
  }, [safetyNumbers])

  const clear = useCallback((peerId: string) => {
    verifiedAtRef.current.delete(peerId)
    setStatuses(prev => {
      if (!prev.has(peerId)) return prev
      const next = new Map(prev)
      next.delete(peerId)
      return next
    })
  }, [])

  const status = useCallback(
    (peerId: string): VerificationStatus => statuses.get(peerId) ?? 'unverified',
    [statuses],
  )

  return { statuses, markVerified, clear, status }
}

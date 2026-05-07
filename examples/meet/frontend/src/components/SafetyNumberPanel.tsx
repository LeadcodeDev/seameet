import { useState, useRef, useEffect } from 'react'
import { useCall } from '@/context/CallContext'
import { ShieldCheck, ShieldAlert, X, Check, AlertTriangle } from 'lucide-react'

export function SafetyNumberPanel() {
  const {
    e2eeEnabled,
    e2eePeerStates,
    e2eeSafetyNumbers,
    remotePeers,
    verificationStatus,
    markPeerVerified,
    clearPeerVerification,
  } = useCall()
  const [open, setOpen] = useState(false)
  const panelRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    function handleClick(e: MouseEvent) {
      if (panelRef.current && !panelRef.current.contains(e.target as Node)) {
        setOpen(false)
      }
    }
    document.addEventListener('mousedown', handleClick)
    return () => document.removeEventListener('mousedown', handleClick)
  }, [open])

  if (!e2eeEnabled) return null

  const allReady = e2eePeerStates.size > 0 && [...e2eePeerStates.values()].every(s => s.ready)
  const someNotReady = e2eePeerStates.size === 0 || [...e2eePeerStates.values()].some(s => !s.ready)
  const hasChangedWarning = [...e2eeSafetyNumbers.keys()].some(id => verificationStatus(id) === 'changed')

  return (
    <div className="relative">
      <button
        data-testid="e2ee-indicator"
        onClick={() => setOpen(!open)}
        className={`flex items-center justify-center h-12 w-12 rounded-full transition-colors ${
          hasChangedWarning
            ? 'bg-red-500/20 text-red-400 hover:bg-red-500/30'
            : someNotReady
            ? 'bg-orange-500/20 text-orange-400 hover:bg-orange-500/30'
            : 'bg-green-500/20 text-green-400 hover:bg-green-500/30'
        }`}
        title={
          hasChangedWarning
            ? 'A verified participant\'s safety number has changed'
            : someNotReady
            ? 'E2EE active — some participants not yet secured'
            : 'End-to-end encrypted'
        }
      >
        {hasChangedWarning ? (
          <AlertTriangle className="h-5 w-5" />
        ) : someNotReady ? (
          <ShieldAlert className="h-5 w-5" />
        ) : (
          <ShieldCheck className="h-5 w-5" />
        )}
      </button>

      {open && (
        <div
          ref={panelRef}
          className="absolute bottom-full mb-2 left-1/2 -translate-x-1/2 w-80 bg-zinc-900 border border-zinc-700 rounded-lg shadow-xl z-50"
        >
          <div className="flex items-center justify-between px-4 py-3 border-b border-zinc-700">
            <h3 className="text-sm font-medium text-zinc-100">E2EE Security</h3>
            <button onClick={() => setOpen(false)} className="text-zinc-400 hover:text-zinc-200">
              <X className="h-4 w-4" />
            </button>
          </div>

          <div className="px-4 py-3 space-y-3 max-h-80 overflow-y-auto">
            <div className="flex items-center gap-2 text-xs text-zinc-400">
              {allReady ? (
                <>
                  <ShieldCheck className="h-3.5 w-3.5 text-green-400" />
                  <span>All participants secured with AES-256-GCM</span>
                </>
              ) : (
                <>
                  <ShieldAlert className="h-3.5 w-3.5 text-orange-400" />
                  <span>Some participants not yet secured</span>
                </>
              )}
            </div>

            {e2eeSafetyNumbers.size > 0 && (
              <div className="space-y-2">
                <p className="text-xs text-zinc-500">
                  Verify safety numbers with each participant out-of-band to confirm identity.
                </p>
                {[...e2eeSafetyNumbers.entries()].map(([peerId, number]) => {
                  const peer = remotePeers.get(peerId)
                  const displayName = peer?.displayName ?? peerId.slice(0, 8)
                  const isReady = e2eePeerStates.get(peerId)?.ready ?? false
                  const status = verificationStatus(peerId)
                  return (
                    <div
                      key={peerId}
                      data-testid="safety-entry"
                      data-peer={peerId}
                      data-verification={status}
                      className={`bg-zinc-800 rounded-md p-2.5 ${
                        status === 'changed' ? 'ring-1 ring-red-500/60' : ''
                      }`}
                    >
                      <div className="flex items-center gap-2 mb-1.5">
                        <div className={`w-2 h-2 rounded-full ${isReady ? 'bg-green-400' : 'bg-orange-400'}`} />
                        <span className="text-xs font-medium text-zinc-200">{displayName}</span>
                        {status === 'verified' && (
                          <span
                            data-testid="verified-badge"
                            className="ml-auto inline-flex items-center gap-1 text-[10px] text-green-400"
                          >
                            <ShieldCheck className="h-3 w-3" />
                            Verified
                          </span>
                        )}
                        {status === 'changed' && (
                          <span
                            data-testid="changed-badge"
                            className="ml-auto inline-flex items-center gap-1 text-[10px] text-red-400"
                          >
                            <AlertTriangle className="h-3 w-3" />
                            Changed
                          </span>
                        )}
                      </div>
                      <p className="font-mono text-[11px] leading-relaxed text-zinc-400 select-all break-all">
                        {number}
                      </p>
                      {status === 'changed' && (
                        <p className="mt-2 text-[11px] text-red-400">
                          The safety number changed since you verified it. Re-confirm out-of-band before trusting again.
                        </p>
                      )}
                      <div className="mt-2 flex gap-2">
                        {status === 'verified' || status === 'changed' ? (
                          <button
                            data-testid="btn-unverify"
                            onClick={() => clearPeerVerification(peerId)}
                            className="text-[11px] px-2 py-1 rounded bg-zinc-700 text-zinc-200 hover:bg-zinc-600"
                          >
                            Clear verification
                          </button>
                        ) : null}
                        {status !== 'verified' && isReady && (
                          <button
                            data-testid="btn-verify"
                            onClick={() => markPeerVerified(peerId)}
                            className="text-[11px] px-2 py-1 rounded bg-green-600/80 text-white hover:bg-green-600 inline-flex items-center gap-1"
                          >
                            <Check className="h-3 w-3" />
                            {status === 'changed' ? 'Re-verify' : 'Mark as verified'}
                          </button>
                        )}
                      </div>
                    </div>
                  )
                })}
              </div>
            )}

            {e2eeSafetyNumbers.size === 0 && (
              <p className="text-xs text-zinc-500 italic">
                Waiting for key exchange to complete...
              </p>
            )}
          </div>
        </div>
      )}
    </div>
  )
}

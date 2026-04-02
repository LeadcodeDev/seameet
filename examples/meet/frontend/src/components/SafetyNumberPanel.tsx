import { useState, useRef, useEffect } from 'react'
import { useCall } from '@/context/CallContext'
import { ShieldCheck, ShieldAlert, X } from 'lucide-react'

export function SafetyNumberPanel() {
  const { e2eeEnabled, e2eePeerStates, e2eeSafetyNumbers, remotePeers } = useCall()
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

  return (
    <div className="relative">
      <button
        data-testid="e2ee-indicator"
        onClick={() => setOpen(!open)}
        className={`flex items-center justify-center h-12 w-12 rounded-full transition-colors ${
          someNotReady
            ? 'bg-orange-500/20 text-orange-400 hover:bg-orange-500/30'
            : 'bg-green-500/20 text-green-400 hover:bg-green-500/30'
        }`}
        title={someNotReady ? 'E2EE active — some participants not yet secured' : 'End-to-end encrypted'}
      >
        {someNotReady ? (
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

          <div className="px-4 py-3 space-y-3 max-h-64 overflow-y-auto">
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
                  return (
                    <div key={peerId} className="bg-zinc-800 rounded-md p-2.5">
                      <div className="flex items-center gap-2 mb-1.5">
                        <div className={`w-2 h-2 rounded-full ${isReady ? 'bg-green-400' : 'bg-orange-400'}`} />
                        <span className="text-xs font-medium text-zinc-200">{displayName}</span>
                      </div>
                      <p className="font-mono text-[11px] leading-relaxed text-zinc-400 select-all break-all">
                        {number}
                      </p>
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

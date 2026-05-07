import { useRef, useEffect, memo } from 'react'
import { MicOff, ShieldCheck, AlertTriangle } from 'lucide-react'
import { Avatar, AvatarFallback } from '@/components/ui/avatar'

type VerificationStatus = 'unverified' | 'verified' | 'changed'

interface VideoTileProps {
  stream: MediaStream | null
  name: string
  isLocal: boolean
  audioEnabled: boolean
  videoEnabled: boolean
  isScreenShare?: boolean
  e2eeActive?: boolean
  /** Worker is fail-closed dropping frames for this tile because no key is
   *  installed yet (handshake in progress). Renders a "waiting for E2EE"
   *  overlay so the user understands why the tile is dark. */
  e2eeNotReady?: boolean
  isActiveSpeaker?: boolean
  verification?: VerificationStatus
}

function getInitials(name: string): string {
  return name
    .split(/\s+/)
    .map((w) => w[0])
    .filter(Boolean)
    .slice(0, 2)
    .join('')
    .toUpperCase()
}

export const VideoTile = memo(function VideoTile({ stream, name, isLocal, audioEnabled, videoEnabled, isScreenShare, e2eeActive, e2eeNotReady, isActiveSpeaker, verification }: VideoTileProps) {
  const videoRef = useRef<HTMLVideoElement>(null)

  useEffect(() => {
    const video = videoRef.current
    if (!video) return

    // Null first to force Chrome to tear down and reinit the decoder
    // even when the same track objects are reused on a new MediaStream.
    video.srcObject = null
    video.srcObject = stream

    if (!stream) return

    const tryPlay = () => { video.play().catch(() => {}) }
    tryPlay()

    // When a track unmutes (RTP starts arriving after reconnection),
    // autoPlay may not re-trigger — force playback.
    // Snapshot tracks at attach time so cleanup always removes the right listeners.
    const tracks = stream.getTracks()
    for (const t of tracks) t.addEventListener('unmute', tryPlay)
    return () => {
      for (const t of tracks) t.removeEventListener('unmute', tryPlay)
    }
  }, [stream])

  const showVideo = isScreenShare || videoEnabled
  const displayLabel = isScreenShare ? `${name}'s screen` : `${name}${isLocal ? ' (You)' : ''}`

  return (
    <div
      data-testid="video-tile"
      data-participant={name}
      data-video={showVideo ? 'on' : 'off'}
      data-verification={verification ?? 'unverified'}
      className={`relative rounded-lg overflow-hidden bg-[#3c4043] flex items-center justify-center ${isScreenShare ? 'ring-2 ring-blue-500/50' : ''} ${isActiveSpeaker ? 'ring-2 ring-green-500' : ''} ${verification === 'changed' ? 'ring-2 ring-red-500' : ''}`}
    >
      {/* Video element */}
      <video
        ref={videoRef}
        data-testid="video-element"
        autoPlay
        playsInline
        muted={isLocal}
        className={`w-full h-full ${isScreenShare ? 'object-contain' : 'object-cover'} ${!showVideo ? 'hidden' : ''}`}
      />

      {/* Avatar fallback when video is off (not for screen share) */}
      {!showVideo && !isScreenShare && (
        <Avatar data-testid="avatar-placeholder" className="h-20 w-20">
          <AvatarFallback className="text-2xl bg-primary text-primary-foreground">
            {getInitials(name)}
          </AvatarFallback>
        </Avatar>
      )}

      {/* Mic-off indicator (not for screen share) */}
      {!audioEnabled && !isScreenShare && (
        <div className="absolute top-2 right-2 bg-black/60 rounded-full p-1">
          <MicOff className="w-4 h-4 text-red-400" />
        </div>
      )}

      {/* E2EE indicator (becomes a verified-badge once the user has confirmed
          the safety number; flips to a warning if it ever changes since). */}
      {e2eeActive && (
        <div
          data-testid="verification-indicator"
          className={`absolute top-2 left-2 bg-black/60 rounded-full p-1 ${
            verification === 'changed' ? 'ring-1 ring-red-500' : ''
          }`}
        >
          {verification === 'changed' ? (
            <AlertTriangle className="w-3 h-3 text-red-400" />
          ) : (
            <ShieldCheck
              className={`w-3 h-3 ${verification === 'verified' ? 'text-emerald-300' : 'text-green-400'}`}
            />
          )}
        </div>
      )}

      {/* Fail-closed E2EE overlay: rendered when the worker is dropping frames
          because no key is installed yet for this participant. */}
      {e2eeNotReady && !isScreenShare && (
        <div
          data-testid="e2ee-not-ready-overlay"
          className="absolute inset-0 flex items-center justify-center bg-black/70 text-white text-xs px-3 text-center"
        >
          <span>Établissement de la session chiffrée…</span>
        </div>
      )}

      {/* Name overlay */}
      <div className="absolute bottom-2 left-2 bg-black/60 rounded px-2 py-0.5 text-xs text-white">
        {displayLabel}
      </div>
    </div>
  )
})

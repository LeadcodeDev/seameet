import { useRef, useEffect } from 'react'
import { MicOff, ShieldCheck } from 'lucide-react'
import { Avatar, AvatarFallback } from '@/components/ui/avatar'

interface VideoTileProps {
  stream: MediaStream | null
  name: string
  isLocal: boolean
  audioEnabled: boolean
  videoEnabled: boolean
  isScreenShare?: boolean
  e2eeActive?: boolean
  isActiveSpeaker?: boolean
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

export function VideoTile({ stream, name, isLocal, audioEnabled, videoEnabled, isScreenShare, e2eeActive, isActiveSpeaker }: VideoTileProps) {
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
      className={`relative rounded-lg overflow-hidden bg-[#3c4043] flex items-center justify-center ${isScreenShare ? 'ring-2 ring-blue-500/50' : ''} ${isActiveSpeaker ? 'ring-2 ring-green-500' : ''}`}
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

      {/* E2EE indicator */}
      {e2eeActive && (
        <div className="absolute top-2 left-2 bg-black/60 rounded-full p-1">
          <ShieldCheck className="w-3 h-3 text-green-400" />
        </div>
      )}

      {/* Name overlay */}
      <div className="absolute bottom-2 left-2 bg-black/60 rounded px-2 py-0.5 text-xs text-white">
        {displayLabel}
      </div>
    </div>
  )
}

import { useRef, useEffect } from 'react'
import { MicOff } from 'lucide-react'
import { Avatar, AvatarFallback } from '@/components/ui/avatar'

interface VideoTileProps {
  stream: MediaStream | null
  name: string
  isLocal: boolean
  audioEnabled: boolean
  videoEnabled: boolean
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

export function VideoTile({ stream, name, isLocal, audioEnabled, videoEnabled }: VideoTileProps) {
  const videoRef = useRef<HTMLVideoElement>(null)

  useEffect(() => {
    if (videoRef.current) {
      videoRef.current.srcObject = stream
    }
  }, [stream])

  return (
    <div className="relative rounded-lg overflow-hidden bg-[#3c4043] flex items-center justify-center">
      {/* Video element */}
      <video
        ref={videoRef}
        autoPlay
        playsInline
        muted={isLocal}
        className={`w-full h-full object-cover ${!videoEnabled ? 'hidden' : ''}`}
      />

      {/* Avatar fallback when video is off */}
      {!videoEnabled && (
        <Avatar className="h-20 w-20">
          <AvatarFallback className="text-2xl bg-primary text-primary-foreground">
            {getInitials(name)}
          </AvatarFallback>
        </Avatar>
      )}

      {/* Mic-off indicator */}
      {!audioEnabled && (
        <div className="absolute top-2 right-2 bg-black/60 rounded-full p-1">
          <MicOff className="w-4 h-4 text-red-400" />
        </div>
      )}

      {/* Name overlay */}
      <div className="absolute bottom-2 left-2 bg-black/60 rounded px-2 py-0.5 text-xs text-white">
        {name} {isLocal && '(You)'}
      </div>
    </div>
  )
}

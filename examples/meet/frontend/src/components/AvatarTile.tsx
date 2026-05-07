import { memo } from 'react'
import { MicOff } from 'lucide-react'
import { Avatar, AvatarFallback } from '@/components/ui/avatar'

interface AvatarTileProps {
  name: string
  audioEnabled: boolean
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

/**
 * Compact tile that never carries a `<video>` element. Used in `large` and
 * `webinar` room modes for participants whose video isn't being rendered,
 * to avoid spawning dozens of decoders the browser can't keep up with.
 */
export const AvatarTile = memo(function AvatarTile({ name, audioEnabled, isActiveSpeaker }: AvatarTileProps) {
  return (
    <div
      data-testid="avatar-tile"
      data-participant={name}
      className={`relative rounded-lg overflow-hidden bg-[#3c4043] flex items-center justify-center ${isActiveSpeaker ? 'ring-2 ring-green-500' : ''}`}
    >
      <Avatar data-testid="avatar-placeholder" className="h-12 w-12">
        <AvatarFallback className="text-base bg-primary text-primary-foreground">
          {getInitials(name)}
        </AvatarFallback>
      </Avatar>

      {!audioEnabled && (
        <div className="absolute top-1 right-1 bg-black/60 rounded-full p-0.5">
          <MicOff className="w-3 h-3 text-red-400" />
        </div>
      )}

      <div className="absolute bottom-1 left-1 bg-black/60 rounded px-1.5 py-0.5 text-[10px] text-white truncate max-w-[calc(100%-0.5rem)]">
        {name}
      </div>
    </div>
  )
})

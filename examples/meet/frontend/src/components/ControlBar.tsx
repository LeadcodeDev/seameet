import { useState, useCallback } from 'react'
import { Mic, MicOff, Video, VideoOff, Monitor, Phone, Copy, Check } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Tooltip, TooltipTrigger, TooltipContent } from '@/components/ui/tooltip'
import { useCall } from '@/context/CallContext'

export function ControlBar() {
  const {
    audioEnabled,
    videoEnabled,
    toggleAudio,
    toggleVideo,
    startScreenShare,
    stopScreenShare,
    screenStream,
    leave,
    roomId,
  } = useCall()

  const [copied, setCopied] = useState(false)

  const handleCopyLink = useCallback(async () => {
    const url = `${window.location.origin}/room/${roomId}`
    await navigator.clipboard.writeText(url)
    setCopied(true)
    setTimeout(() => setCopied(false), 2000)
  }, [roomId])

  const handleScreenShare = useCallback(async () => {
    if (screenStream) {
      stopScreenShare()
    } else {
      try {
        await startScreenShare()
      } catch {
        // User cancelled or error
      }
    }
  }, [screenStream, startScreenShare, stopScreenShare])

  return (
    <div className="flex items-center justify-center gap-3 p-4">
      {/* Mic toggle */}
      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            variant={audioEnabled ? 'secondary' : 'destructive'}
            size="icon"
            className="h-12 w-12 rounded-full"
            onClick={toggleAudio}
          >
            {audioEnabled ? <Mic className="h-5 w-5" /> : <MicOff className="h-5 w-5" />}
          </Button>
        </TooltipTrigger>
        <TooltipContent>{audioEnabled ? 'Mute' : 'Unmute'}</TooltipContent>
      </Tooltip>

      {/* Camera toggle */}
      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            variant={videoEnabled ? 'secondary' : 'destructive'}
            size="icon"
            className="h-12 w-12 rounded-full"
            onClick={toggleVideo}
          >
            {videoEnabled ? <Video className="h-5 w-5" /> : <VideoOff className="h-5 w-5" />}
          </Button>
        </TooltipTrigger>
        <TooltipContent>{videoEnabled ? 'Turn off camera' : 'Turn on camera'}</TooltipContent>
      </Tooltip>

      {/* Screen share */}
      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            variant={screenStream ? 'default' : 'secondary'}
            size="icon"
            className="h-12 w-12 rounded-full"
            onClick={handleScreenShare}
          >
            <Monitor className="h-5 w-5" />
          </Button>
        </TooltipTrigger>
        <TooltipContent>{screenStream ? 'Stop sharing' : 'Share screen'}</TooltipContent>
      </Tooltip>

      {/* Copy room link */}
      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            variant="secondary"
            size="icon"
            className="h-12 w-12 rounded-full"
            onClick={handleCopyLink}
          >
            {copied ? <Check className="h-5 w-5" /> : <Copy className="h-5 w-5" />}
          </Button>
        </TooltipTrigger>
        <TooltipContent>{copied ? 'Copied!' : 'Copy room link'}</TooltipContent>
      </Tooltip>

      {/* Leave */}
      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            variant="destructive"
            className="h-12 rounded-full px-6"
            onClick={leave}
          >
            <Phone className="h-5 w-5 rotate-[135deg]" />
          </Button>
        </TooltipTrigger>
        <TooltipContent>Leave call</TooltipContent>
      </Tooltip>
    </div>
  )
}

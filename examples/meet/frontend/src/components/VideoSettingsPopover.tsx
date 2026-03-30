import { useState, useRef, useEffect, useCallback } from 'react'
import { Settings } from 'lucide-react'
import { Button } from '@/components/ui/button'
import { Tooltip, TooltipTrigger, TooltipContent } from '@/components/ui/tooltip'
import { useCall } from '@/context/CallContext'
import type { VideoSettings } from '@/hooks/useMediaDevices'

const RESOLUTION_PRESETS: { label: string; width: number; height: number }[] = [
  { label: '360p', width: 640, height: 360 },
  { label: '480p', width: 640, height: 480 },
  { label: '720p', width: 1280, height: 720 },
  { label: '1080p', width: 1920, height: 1080 },
]

const FPS_OPTIONS = [15, 24, 30]

export function VideoSettingsPopover() {
  const { videoSettings, updateVideoSettings } = useCall()
  const [open, setOpen] = useState(false)
  const popoverRef = useRef<HTMLDivElement>(null)

  useEffect(() => {
    if (!open) return
    function handleClick(e: MouseEvent) {
      if (popoverRef.current && !popoverRef.current.contains(e.target as Node)) {
        setOpen(false)
      }
    }
    document.addEventListener('mousedown', handleClick)
    return () => document.removeEventListener('mousedown', handleClick)
  }, [open])

  const handleResolutionChange = useCallback((e: React.ChangeEvent<HTMLSelectElement>) => {
    const preset = RESOLUTION_PRESETS.find(p => p.label === e.target.value)
    if (!preset) return
    const next: VideoSettings = { width: preset.width, height: preset.height, frameRate: videoSettings.frameRate }
    updateVideoSettings(next)
  }, [videoSettings.frameRate, updateVideoSettings])

  const handleFpsChange = useCallback((e: React.ChangeEvent<HTMLSelectElement>) => {
    const next: VideoSettings = { ...videoSettings, frameRate: Number(e.target.value) }
    updateVideoSettings(next)
  }, [videoSettings, updateVideoSettings])

  const currentLabel = RESOLUTION_PRESETS.find(p => p.height === videoSettings.height)?.label ?? '480p'

  return (
    <div className="relative" ref={popoverRef}>
      <Tooltip>
        <TooltipTrigger asChild>
          <Button
            variant="secondary"
            size="icon"
            className="h-12 w-12 rounded-full"
            onClick={() => setOpen(o => !o)}
          >
            <Settings className="h-5 w-5" />
          </Button>
        </TooltipTrigger>
        <TooltipContent>Video settings</TooltipContent>
      </Tooltip>

      {open && (
        <div className="absolute bottom-full left-1/2 -translate-x-1/2 mb-2 w-48 rounded-lg border bg-popover p-3 shadow-md space-y-3">
          <div>
            <label className="text-xs font-medium text-muted-foreground">Resolution</label>
            <select
              value={currentLabel}
              onChange={handleResolutionChange}
              className="mt-1 w-full rounded-md border bg-background px-2 py-1.5 text-sm"
            >
              {RESOLUTION_PRESETS.map(p => (
                <option key={p.label} value={p.label}>{p.label}</option>
              ))}
            </select>
          </div>
          <div>
            <label className="text-xs font-medium text-muted-foreground">Frame rate</label>
            <select
              value={videoSettings.frameRate}
              onChange={handleFpsChange}
              className="mt-1 w-full rounded-md border bg-background px-2 py-1.5 text-sm"
            >
              {FPS_OPTIONS.map(fps => (
                <option key={fps} value={fps}>{fps} fps</option>
              ))}
            </select>
          </div>
        </div>
      )}
    </div>
  )
}

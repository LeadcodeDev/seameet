import { useState, useEffect, useCallback, useRef } from 'react'

export interface VideoSettings {
  width: number
  height: number
  frameRate: number
}

const DEFAULT_VIDEO_SETTINGS: VideoSettings = { width: 640, height: 480, frameRate: 24 }

export interface UseMediaDevicesOptions {
  onScreenShareEnded?: () => void
  initialAudioEnabled?: boolean
  initialVideoEnabled?: boolean
}

export interface UseMediaDevicesReturn {
  localStream: MediaStream | null
  audioEnabled: boolean
  videoEnabled: boolean
  videoSettings: VideoSettings
  toggleAudio: () => Promise<void>
  toggleVideo: () => Promise<void>
  updateVideoSettings: (settings: VideoSettings) => void
  startScreenShare: () => Promise<MediaStream>
  stopScreenShare: () => void
  screenStream: MediaStream | null
  error: string | null
}

export function useMediaDevices(options?: UseMediaDevicesOptions): UseMediaDevicesReturn {
  const [localStream, setLocalStream] = useState<MediaStream | null>(null)
  const [audioEnabled, setAudioEnabled] = useState(options?.initialAudioEnabled ?? false)
  const [videoEnabled, setVideoEnabled] = useState(options?.initialVideoEnabled ?? false)
  const [videoSettings, setVideoSettings] = useState<VideoSettings>(DEFAULT_VIDEO_SETTINGS)
  const [screenStream, setScreenStream] = useState<MediaStream | null>(null)
  const [error, setError] = useState<string | null>(null)
  const streamRef = useRef<MediaStream | null>(null)
  const acquiringRef = useRef<Promise<MediaStream | null> | null>(null)
  const onScreenShareEndedRef = useRef(options?.onScreenShareEnded)
  onScreenShareEndedRef.current = options?.onScreenShareEnded

  // Cleanup on unmount: stop all tracks
  useEffect(() => {
    return () => {
      if (streamRef.current) {
        streamRef.current.getTracks().forEach((t) => t.stop())
      }
    }
  }, [])

  const acquireStream = useCallback(async (): Promise<MediaStream | null> => {
    // Already have a stream
    if (streamRef.current) return streamRef.current

    // Guard against concurrent calls
    if (acquiringRef.current) return acquiringRef.current

    const promise = (async () => {
      try {
        const stream = await navigator.mediaDevices.getUserMedia({
          audio: true,
          video: {
            width: { ideal: 640 },
            height: { ideal: 480 },
            frameRate: { ideal: 24, max: 30 },
          },
        })
        // Disable all tracks immediately — callers will enable what they need
        stream.getTracks().forEach((t) => { t.enabled = false })
        streamRef.current = stream
        setLocalStream(stream)
        return stream
      } catch (e) {
        setError(e instanceof Error ? e.message : 'Failed to access media devices')
        return null
      } finally {
        acquiringRef.current = null
      }
    })()

    acquiringRef.current = promise
    return promise
  }, [])

  // Auto-acquire stream on mount if initial media state requests it
  const initialAudioRef = useRef(options?.initialAudioEnabled ?? false)
  const initialVideoRef = useRef(options?.initialVideoEnabled ?? false)
  useEffect(() => {
    if (!initialAudioRef.current && !initialVideoRef.current) return
    let cancelled = false
    ;(async () => {
      const stream = await acquireStream()
      if (!stream || cancelled) return
      if (initialVideoRef.current) {
        stream.getVideoTracks().forEach((t) => { t.enabled = true })
      }
      if (initialAudioRef.current) {
        stream.getAudioTracks().forEach((t) => { t.enabled = true })
      }
    })()
    return () => { cancelled = true }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  const toggleVideo = useCallback(async () => {
    if (videoEnabled) {
      // Turning off
      if (streamRef.current) {
        streamRef.current.getVideoTracks().forEach((t) => { t.enabled = false })
      }
      setVideoEnabled(false)
    } else {
      // Turning on — acquire stream if needed
      const stream = await acquireStream()
      if (stream) {
        stream.getVideoTracks().forEach((t) => { t.enabled = true })
        setVideoEnabled(true)
      }
    }
  }, [videoEnabled, acquireStream])

  const toggleAudio = useCallback(async () => {
    if (audioEnabled) {
      // Turning off
      if (streamRef.current) {
        streamRef.current.getAudioTracks().forEach((t) => { t.enabled = false })
      }
      setAudioEnabled(false)
    } else {
      // Turning on — acquire stream if needed
      const stream = await acquireStream()
      if (stream) {
        stream.getAudioTracks().forEach((t) => { t.enabled = true })
        setAudioEnabled(true)
      }
    }
  }, [audioEnabled, acquireStream])

  const updateVideoSettings = useCallback((settings: VideoSettings) => {
    setVideoSettings(settings)
    if (streamRef.current) {
      const videoTrack = streamRef.current.getVideoTracks()[0]
      if (videoTrack) {
        videoTrack.applyConstraints({
          width: { ideal: settings.width },
          height: { ideal: settings.height },
          frameRate: { ideal: settings.frameRate },
        })
      }
    }
  }, [])

  const startScreenShare = useCallback(async () => {
    const stream = await navigator.mediaDevices.getDisplayMedia({ video: true })
    setScreenStream(stream)
    // Auto-stop when user clicks "Stop sharing" in browser UI
    stream.getVideoTracks()[0]?.addEventListener('ended', () => {
      setScreenStream(null)
      onScreenShareEndedRef.current?.()
    })
    return stream
  }, [])

  const stopScreenShare = useCallback(() => {
    if (screenStream) {
      screenStream.getTracks().forEach((t) => t.stop())
      setScreenStream(null)
    }
  }, [screenStream])

  return {
    localStream,
    audioEnabled,
    videoEnabled,
    videoSettings,
    toggleAudio,
    toggleVideo,
    updateVideoSettings,
    startScreenShare,
    stopScreenShare,
    screenStream,
    error,
  }
}

import { useState, useEffect, useCallback, useRef } from 'react'

export interface UseMediaDevicesOptions {
  onScreenShareEnded?: () => void
}

export interface UseMediaDevicesReturn {
  localStream: MediaStream | null
  audioEnabled: boolean
  videoEnabled: boolean
  toggleAudio: () => void
  toggleVideo: () => void
  startScreenShare: () => Promise<MediaStream>
  stopScreenShare: () => void
  screenStream: MediaStream | null
  error: string | null
}

export function useMediaDevices(options?: UseMediaDevicesOptions): UseMediaDevicesReturn {
  const [localStream, setLocalStream] = useState<MediaStream | null>(null)
  const [audioEnabled, setAudioEnabled] = useState(true)
  const [videoEnabled, setVideoEnabled] = useState(true)
  const [screenStream, setScreenStream] = useState<MediaStream | null>(null)
  const [error, setError] = useState<string | null>(null)
  const mountedRef = useRef(true)
  const onScreenShareEndedRef = useRef(options?.onScreenShareEnded)
  onScreenShareEndedRef.current = options?.onScreenShareEnded

  useEffect(() => {
    mountedRef.current = true
    let stream: MediaStream | null = null

    async function init() {
      try {
        stream = await navigator.mediaDevices.getUserMedia({
          audio: true,
          video: true,
        })
        if (mountedRef.current) {
          setLocalStream(stream)
        } else {
          stream.getTracks().forEach((t) => t.stop())
        }
      } catch (e) {
        if (mountedRef.current) {
          setError(e instanceof Error ? e.message : 'Failed to access media devices')
        }
      }
    }

    init()

    return () => {
      mountedRef.current = false
      if (stream) {
        stream.getTracks().forEach((t) => t.stop())
      }
    }
  }, [])

  const toggleAudio = useCallback(() => {
    if (!localStream) return
    const enabled = !audioEnabled
    localStream.getAudioTracks().forEach((t) => { t.enabled = enabled })
    setAudioEnabled(enabled)
  }, [localStream, audioEnabled])

  const toggleVideo = useCallback(() => {
    if (!localStream) return
    const enabled = !videoEnabled
    localStream.getVideoTracks().forEach((t) => { t.enabled = enabled })
    setVideoEnabled(enabled)
  }, [localStream, videoEnabled])

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
    toggleAudio,
    toggleVideo,
    startScreenShare,
    stopScreenShare,
    screenStream,
    error,
  }
}

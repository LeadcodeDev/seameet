import { createContext, useContext, useEffect, useCallback, useRef, type ReactNode } from 'react'
import { useNavigate } from 'react-router-dom'
import { useSignaling } from '@/hooks/useSignaling'
import { useMediaDevices, type VideoSettings } from '@/hooks/useMediaDevices'
import { useWebRTC, type RemotePeer } from '@/hooks/useWebRTC'
import type { SignalingMessage } from '@/types'

interface CallContextValue {
  participantId: string
  displayName: string
  roomId: string
  localStream: MediaStream | null
  remotePeers: Map<string, RemotePeer>
  audioEnabled: boolean
  videoEnabled: boolean
  videoSettings: VideoSettings
  toggleAudio: () => Promise<void>
  toggleVideo: () => Promise<void>
  updateVideoSettings: (settings: VideoSettings) => void
  startScreenShare: () => Promise<void>
  stopScreenShare: () => Promise<void>
  localScreenStream: MediaStream | null
  connectionState: RTCPeerConnectionState
  signalingState: 'connecting' | 'open' | 'closed'
  leave: () => void
}

const CallContext = createContext<CallContextValue | null>(null)

interface CallProviderProps {
  participantId: string
  displayName: string
  roomId: string
  initialAudioEnabled?: boolean
  initialVideoEnabled?: boolean
  children: ReactNode
}

export function CallProvider({ participantId, displayName, roomId, initialAudioEnabled, initialVideoEnabled, children }: CallProviderProps) {
  const navigate = useNavigate()
  const joinedRef = useRef(false)

  // Ref-based message routing: useSignaling → useWebRTC
  const webrtcHandlerRef = useRef<(msg: SignalingMessage) => void>(() => {})

  const signaling = useSignaling({
    onMessage: (msg) => {
      webrtcHandlerRef.current(msg)
    },
  })

  // Ref to call webrtc.stopScreenShare when browser native stop fires
  const webrtcScreenStopRef = useRef<() => Promise<void>>(async () => {})

  const media = useMediaDevices({
    onScreenShareEnded: () => {
      webrtcScreenStopRef.current()
    },
    initialAudioEnabled,
    initialVideoEnabled,
  })

  const webrtc = useWebRTC({
    participantId,
    roomId,
    localStream: media.localStream,
    signaling,
    videoSettings: media.videoSettings,
  })

  // Wire WebRTC handler — updated every render (safe, no side effects)
  webrtcHandlerRef.current = webrtc.handleMessage
  webrtcScreenStopRef.current = webrtc.stopScreenShare

  // Stable refs for initial media state to avoid stale closures in the join effect
  const initialVideoRef = useRef(initialVideoEnabled)
  const initialAudioRef = useRef(initialAudioEnabled)

  // Join room as soon as signaling is open — no need to wait for localStream.
  useEffect(() => {
    if (signaling.state === 'open' && !joinedRef.current) {
      joinedRef.current = true
      console.log(`[CallContext] joining room ${roomId} as ${participantId.slice(0, 8)}`)
      signaling.join(participantId, roomId, displayName)
      // Signal initial mute state based on lobby selection
      signaling.send({ type: initialVideoRef.current ? 'unmute_video' : 'mute_video', from: participantId, room_id: roomId })
      signaling.send({ type: initialAudioRef.current ? 'unmute_audio' : 'mute_audio', from: participantId, room_id: roomId })
    }
  }, [signaling.state, signaling, participantId, roomId, displayName])

  // Reset joinedRef when signaling reconnects
  useEffect(() => {
    if (signaling.state === 'closed') {
      joinedRef.current = false
    }
  }, [signaling.state])

  // When localStream transitions from null to a real stream, replace the dummy
  // transceiver tracks so the SFU receives real media without renegotiation.
  const prevLocalStreamRef = useRef<MediaStream | null>(null)
  useEffect(() => {
    if (media.localStream && !prevLocalStreamRef.current) {
      webrtc.replaceLocalTracks(media.localStream)
    }
    prevLocalStreamRef.current = media.localStream
  }, [media.localStream, webrtc])

  const handleToggleAudio = useCallback(async () => {
    const willBeMuted = media.audioEnabled
    await media.toggleAudio()
    if (willBeMuted) {
      signaling.send({ type: 'mute_audio', from: participantId, room_id: roomId })
    } else {
      signaling.send({ type: 'unmute_audio', from: participantId, room_id: roomId })
    }
  }, [media, signaling, participantId, roomId])

  const handleToggleVideo = useCallback(async () => {
    const willBeMuted = media.videoEnabled
    await media.toggleVideo()
    if (willBeMuted) {
      signaling.send({ type: 'mute_video', from: participantId, room_id: roomId })
    } else {
      signaling.send({ type: 'unmute_video', from: participantId, room_id: roomId })
      if (media.localStream) {
        await webrtc.replaceLocalTracks(media.localStream)
      }
    }
  }, [media, signaling, participantId, roomId, webrtc])

  const handleUpdateVideoSettings = useCallback((settings: VideoSettings) => {
    media.updateVideoSettings(settings)
    signaling.send({
      type: 'video_config_changed',
      from: participantId,
      room_id: roomId,
      width: settings.width,
      height: settings.height,
      fps: settings.frameRate,
    })
  }, [media, signaling, participantId, roomId])

  const handleStartScreenShare = useCallback(async () => {
    const stream = await media.startScreenShare()
    await webrtc.startScreenShare(stream)
  }, [media, webrtc])

  const handleStopScreenShare = useCallback(async () => {
    media.stopScreenShare()
    await webrtc.stopScreenShare()
  }, [media, webrtc])

  const leave = useCallback(() => {
    signaling.close()
    navigate('/')
  }, [signaling, navigate])

  const value: CallContextValue = {
    participantId,
    displayName,
    roomId,
    localStream: media.localStream,
    remotePeers: webrtc.remotePeers,
    audioEnabled: media.audioEnabled,
    videoEnabled: media.videoEnabled,
    videoSettings: media.videoSettings,
    toggleAudio: handleToggleAudio,
    toggleVideo: handleToggleVideo,
    updateVideoSettings: handleUpdateVideoSettings,
    startScreenShare: handleStartScreenShare,
    stopScreenShare: handleStopScreenShare,
    localScreenStream: webrtc.localScreenStream,
    connectionState: webrtc.connectionState,
    signalingState: signaling.state,
    leave,
  }

  return (
    <CallContext.Provider value={value}>
      {children}
    </CallContext.Provider>
  )
}

export function useCall(): CallContextValue {
  const ctx = useContext(CallContext)
  if (!ctx) {
    throw new Error('useCall must be used within a CallProvider')
  }
  return ctx
}

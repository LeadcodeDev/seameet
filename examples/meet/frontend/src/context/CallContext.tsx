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
  toggleAudio: () => void
  toggleVideo: () => void
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
  children: ReactNode
}

export function CallProvider({ participantId, displayName, roomId, children }: CallProviderProps) {
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

  // Join room when BOTH signaling is open AND localStream is available.
  // This matches the browser-demo behavior: getUserMedia THEN connect signaling.
  // Without this, createOfferToServer runs with localStream=null and creates
  // dummy transceivers that never send real tracks to the SFU.
  useEffect(() => {
    if (signaling.state === 'open' && media.localStream && !joinedRef.current) {
      joinedRef.current = true
      console.log(`[CallContext] joining room ${roomId} as ${participantId.slice(0, 8)} (localStream ready)`)
      signaling.join(participantId, roomId, displayName)
    }
  }, [signaling.state, media.localStream, signaling, participantId, roomId, displayName])

  // Reset joinedRef when signaling reconnects
  useEffect(() => {
    if (signaling.state === 'closed') {
      joinedRef.current = false
    }
  }, [signaling.state])

  const handleToggleAudio = useCallback(() => {
    const willBeMuted = media.audioEnabled
    media.toggleAudio()
    if (willBeMuted) {
      signaling.send({ type: 'mute_audio', from: participantId, room_id: roomId })
    } else {
      signaling.send({ type: 'unmute_audio', from: participantId, room_id: roomId })
    }
  }, [media, signaling, participantId, roomId])

  const handleToggleVideo = useCallback(() => {
    const willBeMuted = media.videoEnabled
    media.toggleVideo()
    if (willBeMuted) {
      signaling.send({ type: 'mute_video', from: participantId, room_id: roomId })
    } else {
      signaling.send({ type: 'unmute_video', from: participantId, room_id: roomId })
    }
  }, [media, signaling, participantId, roomId])

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
    // Stop screen share if active before leaving
    if (webrtc.localScreenStream) {
      media.stopScreenShare()
      // Send screen_share_stopped so other peers clean up the tile
      signaling.send({
        type: 'screen_share_stopped',
        from: participantId,
        room_id: roomId,
        track_id: 0,
      })
    }
    signaling.send({
      type: 'leave',
      participant: participantId,
      room_id: roomId,
    })
    navigate('/')
  }, [signaling, participantId, roomId, navigate, webrtc.localScreenStream, media])

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

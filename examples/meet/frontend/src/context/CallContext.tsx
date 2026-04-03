import { createContext, useContext, useEffect, useCallback, useRef, useState, type ReactNode } from 'react'
import { useNavigate } from 'react-router-dom'
import { useSignaling } from '@/hooks/useSignaling'
import { useMediaDevices, type VideoSettings } from '@/hooks/useMediaDevices'
import { useWebRTC, type RemotePeer } from '@/hooks/useWebRTC'
import { useE2EE, type E2EEPeerState } from '@/hooks/useE2EE'
import type { ChatMessage } from '@/components/ChatPanel'
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
  e2eeEnabled: boolean
  e2eePeerStates: Map<string, E2EEPeerState>
  e2eeSafetyNumbers: Map<string, string>
  chatMessages: ChatMessage[]
  sendChatMessage: (content: string) => void
  activeSpeakerId: string | null
  mediaError: string | null
}

const CallContext = createContext<CallContextValue | null>(null)

interface CallProviderProps {
  participantId: string
  displayName: string
  roomId: string
  initialAudioEnabled?: boolean
  initialVideoEnabled?: boolean
  initialE2EEEnabled?: boolean
  children: ReactNode
}

export function CallProvider({ participantId, displayName, roomId, initialAudioEnabled, initialVideoEnabled, initialE2EEEnabled, children }: CallProviderProps) {
  const navigate = useNavigate()
  const joinedRef = useRef(false)
  const [chatMessages, setChatMessages] = useState<ChatMessage[]>([])
  const [activeSpeakerId, setActiveSpeakerId] = useState<string | null>(null)

  // Ref-based message routing: useSignaling → useWebRTC
  const webrtcHandlerRef = useRef<(msg: SignalingMessage) => void>(() => {})

  const signaling = useSignaling({
    onMessage: (msg) => {
      // Route E2EE signaling messages to the E2EE hook
      if (msg.type === 'e2ee_public_key' || msg.type === 'e2ee_sender_key' || msg.type === 'e2ee_key_rotation') {
        e2eeHandlerRef.current(msg)
        return
      }
      // Handle chat messages
      if (msg.type === 'chat_message') {
        ;(async () => {
          let content = msg.content
          let encrypted = false
          if (msg.encrypted) {
            const decrypted = await e2eeRef.current.decryptChat(msg.from, msg.content, msg.key_id ?? 0)
            if (decrypted !== null) {
              content = decrypted
            } else {
              content = '\u{1F512} Message chiffré'
              encrypted = true
            }
          }
          setChatMessages(prev => {
            const next = [...prev, {
              id: `${msg.from}-${msg.timestamp}`,
              from: msg.from,
              displayName: msg.display_name ?? msg.from.slice(0, 8),
              content,
              timestamp: msg.timestamp,
              encrypted,
            }]
            // Keep only the most recent 500 messages to avoid unbounded growth
            return next.length > 500 ? next.slice(-500) : next
          })
        })()
        return
      }
      // Handle active speaker
      if (msg.type === 'active_speaker') {
        setActiveSpeakerId(msg.speaker)
        return
      }
      webrtcHandlerRef.current(msg)
    },
  })

  // E2EE hook
  const e2ee = useE2EE({
    enabled: initialE2EEEnabled ?? false,
    participantId,
    roomId,
    signaling,
  })

  // Ref for e2ee handler to avoid stale closures
  const e2eeHandlerRef = useRef<(msg: SignalingMessage) => void>(() => {})
  e2eeHandlerRef.current = e2ee.handleMessage

  // Ref for e2ee encrypt/decrypt to avoid stale closures in async chat handler
  const e2eeRef = useRef(e2ee)
  e2eeRef.current = e2ee

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
    e2eeWorker: e2ee.worker,
    e2eeEnabled: e2ee.enabled,
  })

  // Wire WebRTC handler — updated every render (safe, no side effects)
  webrtcHandlerRef.current = webrtc.handleMessage
  webrtcScreenStopRef.current = webrtc.stopScreenShare

  // Refs tracking the *current* media state so the join effect always sends
  // the correct mute signals — both on first join and on WS reconnection.
  const videoEnabledRef = useRef(initialVideoEnabled ?? false)
  const audioEnabledRef = useRef(initialAudioEnabled ?? false)
  videoEnabledRef.current = media.videoEnabled
  audioEnabledRef.current = media.audioEnabled

  // Join room once signaling is open AND media has been acquired (or failed) —
  // ensures createOfferToServer receives real tracks when available.
  useEffect(() => {
    if (signaling.state === 'open' && media.mediaReady && !joinedRef.current) {
      joinedRef.current = true
      console.log(`[CallContext] joining room ${roomId} as ${participantId.slice(0, 8)}`)
      signaling.join(participantId, roomId, displayName)
      // Signal current mute state (correct on first join and on reconnection)
      signaling.send({ type: videoEnabledRef.current ? 'unmute_video' : 'mute_video', from: participantId, room_id: roomId })
      signaling.send({ type: audioEnabledRef.current ? 'unmute_audio' : 'mute_audio', from: participantId, room_id: roomId })
    }
  }, [signaling.state, signaling, media.mediaReady, participantId, roomId, displayName])

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

  // Track E2EE peer lifecycle via remotePeers changes.
  const prevRemotePeerIdsRef = useRef<Set<string>>(new Set())
  useEffect(() => {
    if (!e2ee.enabled) return
    const currentIds = new Set(webrtc.remotePeers.keys())
    const prevIds = prevRemotePeerIdsRef.current

    // New peers
    for (const id of currentIds) {
      if (!prevIds.has(id)) {
        e2ee.onPeerJoined(id)
      }
    }
    // Left peers
    for (const id of prevIds) {
      if (!currentIds.has(id)) {
        e2ee.onPeerLeft(id)
      }
    }
    prevRemotePeerIdsRef.current = currentIds
  }, [webrtc.remotePeers, e2ee])

  const handleSendChatMessage = useCallback((content: string) => {
    if (e2eeRef.current.enabled) {
      e2eeRef.current.encryptChat(content).then((result) => {
        if (result) {
          signaling.send({
            type: 'chat_message',
            from: participantId,
            room_id: roomId,
            display_name: displayName,
            content: result.ciphertext,
            timestamp: Date.now(),
            encrypted: true,
            key_id: result.keyId,
          } as SignalingMessage)
        } else {
          // Fallback to plaintext if encryption fails
          signaling.sendChatMessage(participantId, roomId, content, displayName)
        }
      })
    } else {
      signaling.sendChatMessage(participantId, roomId, content, displayName)
    }
  }, [signaling, participantId, roomId, displayName])

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
    e2eeEnabled: e2ee.enabled,
    e2eePeerStates: e2ee.peerStates,
    e2eeSafetyNumbers: e2ee.safetyNumbers,
    chatMessages,
    sendChatMessage: handleSendChatMessage,
    activeSpeakerId,
    mediaError: media.error,
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

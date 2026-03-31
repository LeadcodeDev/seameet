import { useRef, useState, useEffect, useCallback } from 'react'
import type { SignalingMessage } from '@/types'
import type { UseSignalingReturn } from '@/hooks/useSignaling'
import type { VideoSettings } from '@/hooks/useMediaDevices'

// Pre-allocate transceiver slots to avoid renegotiation when peers join.
// str0m's accept_offer in RTP mode does not support renegotiation reliably,
// so the initial SDP offer includes enough audio+video pairs for future peers.
const MAX_PEER_SLOTS = 7

function getBitrate(height: number, fps: number): number {
  const base: Record<number, number> = { 360: 500_000, 480: 800_000, 720: 1_500_000, 1080: 3_000_000 }
  const bitrate = base[height] ?? 800_000
  return fps > 30 ? Math.round(bitrate * 1.6) : bitrate
}

interface TransceiverSlot {
  audioTransceiver: RTCRtpTransceiver
  videoTransceiver: RTCRtpTransceiver
}

export interface RemotePeer {
  id: string
  displayName: string
  stream: MediaStream
  audioMid: string | null
  videoMid: string | null
  audioMuted: boolean
  videoMuted: boolean
  screenTransceiver: RTCRtpTransceiver | null
  screenStream: MediaStream | null
  videoConfig?: { width: number; height: number; fps: number }
}

export interface UseWebRTCOptions {
  participantId: string
  roomId: string
  localStream: MediaStream | null
  signaling: UseSignalingReturn
  videoSettings: VideoSettings
}

export interface UseWebRTCReturn {
  remotePeers: Map<string, RemotePeer>
  connectionState: RTCPeerConnectionState
  handleMessage: (msg: SignalingMessage) => void
  startScreenShare: (screenStream: MediaStream) => Promise<void>
  stopScreenShare: () => Promise<void>
  localScreenStream: MediaStream | null
  replaceLocalTracks: (stream: MediaStream) => Promise<void>
}

export function useWebRTC({
  participantId,
  roomId,
  localStream,
  signaling,
  videoSettings,
}: UseWebRTCOptions): UseWebRTCReturn {
  const pcRef = useRef<RTCPeerConnection | null>(null)
  const remotePeersRef = useRef<Map<string, RemotePeer>>(new Map())
  const [remotePeers, setRemotePeers] = useState<Map<string, RemotePeer>>(new Map())
  const [connectionState, setConnectionState] = useState<RTCPeerConnectionState>('new')

  const renegotiatingRef = useRef(false)
  const renegotiationPendingRef = useRef(false)
  const renegotiationTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const disconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const screenTransceiverRef = useRef<RTCRtpTransceiver | null>(null)
  const localAudioTransceiverRef = useRef<RTCRtpTransceiver | null>(null)
  const localVideoTransceiverRef = useRef<RTCRtpTransceiver | null>(null)
  const [localScreenStream, setLocalScreenStream] = useState<MediaStream | null>(null)

  // Pool of pre-allocated transceiver pairs from the initial offer.
  const transceiverPoolRef = useRef<TransceiverSlot[]>([])

  // Message queue to serialize async message processing (like browser-demo's await)
  const messageQueueRef = useRef<SignalingMessage[]>([])
  const processingRef = useRef(false)

  // Keep refs for values needed in callbacks to avoid stale closures
  const participantIdRef = useRef(participantId)
  const roomIdRef = useRef(roomId)
  const localStreamRef = useRef(localStream)
  const signalingRef = useRef(signaling)

  const videoSettingsRef = useRef(videoSettings)

  participantIdRef.current = participantId
  roomIdRef.current = roomId
  localStreamRef.current = localStream
  signalingRef.current = signaling
  videoSettingsRef.current = videoSettings

  const updateRemotePeersState = useCallback(() => {
    setRemotePeers(new Map(remotePeersRef.current))
  }, [])

  const removeRemotePeer = useCallback((peerId: string) => {
    const info = remotePeersRef.current.get(peerId)
    if (!info) return

    // Clear stream tracks but do NOT set transceivers to inactive —
    // the mids must stay active in str0m for reuse.
    for (const track of info.stream.getTracks()) {
      info.stream.removeTrack(track)
    }
    // Find the transceiver pair by mid and return to pool.
    const pc = pcRef.current
    if (pc && info.audioMid && info.videoMid) {
      const transceivers = pc.getTransceivers()
      const audioT = transceivers.find(t => t.mid === info.audioMid)
      const videoT = transceivers.find(t => t.mid === info.videoMid)
      if (audioT && videoT) {
        transceiverPoolRef.current.unshift({ audioTransceiver: audioT, videoTransceiver: videoT })
      }
    }

    remotePeersRef.current.delete(peerId)
    updateRemotePeersState()
    console.log(`[WebRTC] removeRemotePeer: ${peerId.slice(0, 8)}, pool: ${transceiverPoolRef.current.length}`)
  }, [updateRemotePeersState])

  const addRemotePeer = useCallback((peerId: string, displayName?: string) => {
    if (remotePeersRef.current.has(peerId)) {
      // Peer reconnected — recycle old entry so the new slot gets fresh media.
      removeRemotePeer(peerId)
    }

    const slot = transceiverPoolRef.current.shift()
    if (!slot) {
      console.warn(`[WebRTC] no free transceiver slots for peer ${peerId.slice(0, 8)}`)
      return
    }

    const stream = new MediaStream()
    const audioMid = slot.audioTransceiver.mid
    const videoMid = slot.videoTransceiver.mid

    // Attach receiver tracks directly from the pre-allocated transceivers.
    // ontrack may have already fired (during setRemoteDescription) before
    // this peer was added, so we cannot rely on ontrack for initial routing.
    const audioTrack = slot.audioTransceiver.receiver.track
    const videoTrack = slot.videoTransceiver.receiver.track
    if (audioTrack) stream.addTrack(audioTrack)
    if (videoTrack) stream.addTrack(videoTrack)

    const peer: RemotePeer = {
      id: peerId,
      displayName: displayName ?? peerId.slice(0, 8),
      stream,
      audioMid,
      videoMid,
      audioMuted: false,
      videoMuted: false,
      screenTransceiver: null,
      screenStream: null,
    }

    remotePeersRef.current.set(peerId, peer)
    updateRemotePeersState()
    console.log(`[WebRTC] addRemotePeer: ${peerId.slice(0, 8)}, mids: audio=${audioMid} video=${videoMid}, tracks: ${stream.getTracks().length}, pool remaining: ${transceiverPoolRef.current.length}`)
  }, [removeRemotePeer, updateRemotePeersState])

  const renegotiate = useCallback(async () => {
    const pc = pcRef.current
    if (!pc) return

    if (renegotiatingRef.current) {
      renegotiationPendingRef.current = true
      console.log('[WebRTC] renegotiation queued (already in progress)')
      return
    }
    renegotiatingRef.current = true

    const offer = await pc.createOffer()
    await pc.setLocalDescription(offer)

    renegotiationTimerRef.current = setTimeout(() => {
      if (renegotiatingRef.current) {
        console.warn('[WebRTC] renegotiation timeout (10s) — resetting')
        renegotiatingRef.current = false
        if (renegotiationPendingRef.current) {
          renegotiationPendingRef.current = false
          renegotiate()
        }
      }
    }, 10000)

    signalingRef.current.sendOffer(
      participantIdRef.current,
      roomIdRef.current,
      offer.sdp!,
    )
    console.log('[WebRTC] renegotiation offer sent')
  }, [])

  const createOfferToServer = useCallback(async (existingPeers: string[], displayNames?: Record<string, string>) => {
    console.log(`[WebRTC] createOfferToServer, existingPeers: ${existingPeers.length}, localStream: ${!!localStreamRef.current}`)

    // Close previous PC and clear stale remote peers (signaling reconnect).
    if (pcRef.current) {
      pcRef.current.close()
      pcRef.current = null
    }
    remotePeersRef.current.clear()
    transceiverPoolRef.current = []
    updateRemotePeersState()

    const pc = new RTCPeerConnection({
      iceServers: [
        { urls: 'stun:stun.l.google.com:19302' },
        { urls: 'stun:stun1.l.google.com:19302' },
      ],
    })
    pcRef.current = pc

    pc.onicecandidate = (evt) => {
      if (!evt.candidate) return
      signalingRef.current.sendIceCandidate(
        participantIdRef.current,
        participantIdRef.current,
        roomIdRef.current,
        evt.candidate,
      )
    }

    pc.ontrack = (evt) => {
      const mid = evt.transceiver.mid
      console.log(`[WebRTC] ontrack: ${evt.track.kind} (mid=${mid})`)
      for (const [peerId, info] of remotePeersRef.current) {
        // Screen share transceiver — match by mid
        if (info.screenTransceiver && mid === info.screenTransceiver.mid) {
          if (!info.screenStream) {
            info.screenStream = new MediaStream()
          }
          info.screenStream.addTrack(evt.track)
          updateRemotePeersState()
          console.log(`[WebRTC] routed screen track to peer ${peerId.slice(0, 8)}`)
          return
        }
        // Audio/video — match by stored mid values
        if (mid === info.audioMid || mid === info.videoMid) {
          const ms = info.stream
          for (const old of ms.getTracks()) {
            if (old.kind === evt.track.kind && old.id !== evt.track.id) {
              ms.removeTrack(old)
            }
          }
          ms.addTrack(evt.track)
          updateRemotePeersState()
          console.log(`[WebRTC] routed ${evt.track.kind} track to peer ${peerId.slice(0, 8)}`)
          return
        }
      }
      console.log(`[WebRTC] unmatched track (mid=${mid})`)
    }

    pc.onconnectionstatechange = () => {
      const state = pc.connectionState
      console.log(`[WebRTC] connectionState: ${state}`)
      setConnectionState(state)

      if (disconnectTimerRef.current) {
        clearTimeout(disconnectTimerRef.current)
        disconnectTimerRef.current = null
      }

      if (state === 'disconnected') {
        disconnectTimerRef.current = setTimeout(() => {
          if (pc.connectionState === 'disconnected') {
            setConnectionState('disconnected')
          }
        }, 3000)
      }
    }

    // Add local tracks or dummy transceivers for send slots
    const stream = localStreamRef.current
    if (stream) {
      stream.getTracks().forEach((track) => {
        pc.addTrack(track, stream)
      })
      // Find local transceivers by track kind
      const transceivers = pc.getTransceivers()
      localAudioTransceiverRef.current = transceivers.find(t => t.sender.track?.kind === 'audio') ?? null
      localVideoTransceiverRef.current = transceivers.find(t => t.sender.track?.kind === 'video') ?? null
      console.log(`[WebRTC] added ${stream.getTracks().length} local tracks`)
    } else {
      localAudioTransceiverRef.current = pc.addTransceiver('audio', { direction: 'sendrecv' })
      localVideoTransceiverRef.current = pc.addTransceiver('video', { direction: 'sendrecv' })
      console.log('[WebRTC] no local media — added dummy transceivers')
    }

    // Pre-allocate transceiver pool so we never renegotiate for new peers.
    const totalSlots = Math.max(MAX_PEER_SLOTS, existingPeers.length)
    const pool: TransceiverSlot[] = []
    for (let i = 0; i < totalSlots; i++) {
      pool.push({
        audioTransceiver: pc.addTransceiver('audio', { direction: 'sendrecv' }),
        videoTransceiver: pc.addTransceiver('video', { direction: 'sendrecv' }),
      })
    }
    console.log(`[WebRTC] pre-allocated ${totalSlots} transceiver pairs`)

    // Create offer FIRST so transceivers get mids assigned.
    const offer = await pc.createOffer()
    await pc.setLocalDescription(offer)

    // Now that mids are assigned, store pool and assign existing peers.
    transceiverPoolRef.current = pool
    for (const peerId of existingPeers) {
      addRemotePeer(peerId, displayNames?.[peerId])
    }

    // Mark as renegotiating BEFORE sending. This prevents the
    // replaceLocalTracks effect from firing a second concurrent offer
    // when localStream arrives while we're still waiting for the initial answer.
    renegotiatingRef.current = true

    signalingRef.current.sendOffer(
      participantIdRef.current,
      roomIdRef.current,
      offer.sdp!,
    )
    console.log('[WebRTC] initial offer sent')
  }, [addRemotePeer, updateRemotePeersState])

  // Async message handler — mirrors browser-demo's async handleSignalingMessage
  const processMessage = useCallback(async (data: SignalingMessage) => {
    if (data.type === 'ready') {
      const peers = data.peers ?? []
      const displayNames = data.display_names
      console.log(`[WebRTC] ready — ${peers.length} existing peer(s)`)
      await createOfferToServer(peers, displayNames)
      return
    }

    if (data.type === 'answer') {
      const pc = pcRef.current
      if (!pc) return

      if (renegotiationTimerRef.current) {
        clearTimeout(renegotiationTimerRef.current)
        renegotiationTimerRef.current = null
      }

      try {
        await pc.setRemoteDescription({ type: 'answer', sdp: data.sdp })
      } catch (e) {
        console.error('[WebRTC] setRemoteDescription failed:', e)
        renegotiatingRef.current = false
        return
      }

      console.log('[WebRTC] answer applied')
      renegotiatingRef.current = false

      if (renegotiationPendingRef.current) {
        renegotiationPendingRef.current = false
        await renegotiate()
      }
      return
    }

    if (data.type === 'ice_candidate') {
      const pc = pcRef.current
      if (!pc) return
      try {
        await pc.addIceCandidate({
          candidate: data.candidate,
          sdpMid: data.sdp_mid ?? null,
          sdpMLineIndex: data.sdp_mline_index ?? null,
        })
      } catch (e) {
        console.warn('[WebRTC] ICE candidate error:', e)
      }
      return
    }

    if (data.type === 'room_status') {
      const myId = participantIdRef.current
      const remoteParticipants = data.participants.filter(p => p.id !== myId)
      const remoteIds = new Set(remoteParticipants.map(p => p.id))

      // Remove peers no longer in the room
      for (const peerId of remotePeersRef.current.keys()) {
        if (!remoteIds.has(peerId)) {
          removeRemotePeer(peerId)
        }
      }

      // Add new peers not yet tracked
      for (const p of remoteParticipants) {
        if (!remotePeersRef.current.has(p.id)) {
          addRemotePeer(p.id, p.display_name)
        }
      }

      // Update media state for all remote peers
      for (const p of remoteParticipants) {
        const info = remotePeersRef.current.get(p.id)
        if (info) {
          info.audioMuted = p.audio_muted
          info.videoMuted = p.video_muted
        }
      }

      updateRemotePeersState()
      return
    }

    if (data.type === 'request_renegotiation') {
      const pc = pcRef.current
      if (!pc) return
      const slots = data.needed_slots
      for (let i = 0; i < slots; i++) {
        transceiverPoolRef.current.push({
          audioTransceiver: pc.addTransceiver('audio', { direction: 'sendrecv' }),
          videoTransceiver: pc.addTransceiver('video', { direction: 'sendrecv' }),
        })
      }
      await renegotiate()
      return
    }

    if (data.type === 'screen_share_started') {
      const peerId = data.from
      const pc = pcRef.current
      if (!pc) return
      const info = remotePeersRef.current.get(peerId)
      if (!info) return
      console.log(`[WebRTC] screen_share_started from ${peerId.slice(0, 8)}`)

      // The SFU routes screen share RTP to the first free video mid in its
      // slot for this source peer.  That mid corresponds to a pre-allocated
      // pool transceiver on this browser — find it by looking for the first
      // video transceiver whose mid isn't already assigned to any peer or
      // used for our own local tracks.
      const usedMids = new Set<string | null>()
      for (const [, peer] of remotePeersRef.current) {
        usedMids.add(peer.audioMid)
        usedMids.add(peer.videoMid)
        if (peer.screenTransceiver) usedMids.add(peer.screenTransceiver.mid)
      }

      let screenTransceiver: RTCRtpTransceiver | null = null
      for (const t of pc.getTransceivers()) {
        if (t.mid === null) continue
        if (t.receiver.track.kind !== 'video') continue
        if (usedMids.has(t.mid)) continue
        // Skip own transceivers (they have a local send track attached)
        if (t.sender.track !== null) continue
        screenTransceiver = t
        break
      }

      if (!screenTransceiver) {
        console.warn(`[WebRTC] no free video transceiver for screen share from ${peerId.slice(0, 8)}`)
        return
      }

      const screenStream = new MediaStream()
      const videoTrack = screenTransceiver.receiver.track
      if (videoTrack) screenStream.addTrack(videoTrack)

      info.screenTransceiver = screenTransceiver
      info.screenStream = screenStream
      updateRemotePeersState()
      console.log(`[WebRTC] screen share routed via mid=${screenTransceiver.mid}`)
      return
    }

    if (data.type === 'video_config_changed') {
      const peer = remotePeersRef.current.get(data.from)
      if (peer) {
        peer.videoConfig = { width: data.width, height: data.height, fps: data.fps }
        updateRemotePeersState()
      }
      return
    }

    if (data.type === 'screen_share_stopped') {
      const peerId = data.from
      const info = remotePeersRef.current.get(peerId)
      if (!info) return
      console.log(`[WebRTC] screen_share_stopped from ${peerId.slice(0, 8)}`)
      // Don't set direction='inactive' — it's a pool transceiver that may be
      // reused for future screen shares.
      info.screenTransceiver = null
      info.screenStream = null
      updateRemotePeersState()
      return
    }
  }, [createOfferToServer, addRemotePeer, removeRemotePeer, renegotiate, updateRemotePeersState])

  const replaceLocalTracks = useCallback(async (stream: MediaStream) => {
    const audioTrack = stream.getAudioTracks()[0] ?? null
    const videoTrack = stream.getVideoTracks()[0] ?? null
    if (localAudioTransceiverRef.current && audioTrack) {
      await localAudioTransceiverRef.current.sender.replaceTrack(audioTrack)
      console.log('[WebRTC] replaced local audio track')
    }
    if (localVideoTransceiverRef.current && videoTrack) {
      await localVideoTransceiverRef.current.sender.replaceTrack(videoTrack)
      console.log('[WebRTC] replaced local video track')
    }
    // No renegotiation needed — replaceTrack sends media on the existing transceiver.
    // Renegotiation during ICE connecting causes str0m to fail ICE.
  }, [])

  const startScreenShare = useCallback(async (screenStream: MediaStream) => {
    const pc = pcRef.current
    if (!pc) return
    const track = screenStream.getVideoTracks()[0]
    if (!track) return
    // Use addTransceiver (NOT addTrack) to guarantee a NEW transceiver.
    const transceiver = pc.addTransceiver(track, { direction: 'sendrecv' })
    screenTransceiverRef.current = transceiver
    setLocalScreenStream(screenStream)
    // Send signal BEFORE renegotiation so the SFU sets screen_share_active
    // before processing the renegotiation that adds the screen mid.
    // WS messages are ordered, so ScreenShareActive arrives at run_media
    // before RenegotiationOffer — the new video mid is then correctly
    // identified as own_screen_mid.
    signalingRef.current.send({
      type: 'screen_share_started',
      from: participantIdRef.current,
      room_id: roomIdRef.current,
      track_id: 0,
    })
    await renegotiate()
    console.log('[WebRTC] screen share started')
  }, [renegotiate])

  const stopScreenShare = useCallback(async () => {
    const pc = pcRef.current
    if (!pc) return
    // Send signal BEFORE renegotiation for same ordering reason.
    signalingRef.current.send({
      type: 'screen_share_stopped',
      from: participantIdRef.current,
      room_id: roomIdRef.current,
      track_id: 0,
    })
    const transceiver = screenTransceiverRef.current
    if (transceiver) {
      transceiver.sender.track?.stop()
      try {
        pc.removeTrack(transceiver.sender)
        transceiver.direction = 'inactive'
      } catch { /* ignore */ }
    }
    screenTransceiverRef.current = null
    setLocalScreenStream(null)
    await renegotiate()
    console.log('[WebRTC] screen share stopped')
  }, [renegotiate])

  // Process message queue sequentially (like browser-demo's await handleSignalingMessage)
  const drainQueue = useCallback(async () => {
    if (processingRef.current) return
    processingRef.current = true
    try {
      while (messageQueueRef.current.length > 0) {
        const msg = messageQueueRef.current.shift()!
        try {
          await processMessage(msg)
        } catch (e) {
          console.error('[WebRTC] processMessage error:', e)
        }
      }
    } finally {
      processingRef.current = false
      // Re-drain if messages arrived between the last while check and finally.
      if (messageQueueRef.current.length > 0) {
        queueMicrotask(() => drainQueue())
      }
    }
  }, [processMessage])

  // Public handler: enqueue message and drain
  const handleMessage = useCallback((msg: SignalingMessage) => {
    messageQueueRef.current.push(msg)
    drainQueue()
  }, [drainQueue])

  // Apply sender encoding parameters on connection and when videoSettings change
  useEffect(() => {
    if (connectionState !== 'connected') return
    const pc = pcRef.current
    if (!pc) return
    try {
      const videoSenders = pc.getSenders().filter(s => s.track?.kind === 'video')
      for (const sender of videoSenders) {
        const params = sender.getParameters()
        if (params.encodings.length > 0) {
          params.encodings[0].maxBitrate = getBitrate(videoSettings.height, videoSettings.frameRate)
          params.encodings[0].maxFramerate = videoSettings.frameRate
          params.degradationPreference = 'maintain-resolution'
          sender.setParameters(params)
        }
      }
    } catch (e) {
      console.warn('[WebRTC] setParameters failed:', e)
    }
  }, [videoSettings, connectionState])

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      if (renegotiationTimerRef.current) {
        clearTimeout(renegotiationTimerRef.current)
      }
      if (disconnectTimerRef.current) {
        clearTimeout(disconnectTimerRef.current)
      }
      // Stop screen share track if active
      const screenTransceiver = screenTransceiverRef.current
      if (screenTransceiver) {
        screenTransceiver.sender.track?.stop()
        screenTransceiverRef.current = null
      }
      if (pcRef.current) {
        pcRef.current.close()
        pcRef.current = null
      }
    }
  }, [])

  return {
    remotePeers,
    connectionState,
    handleMessage,
    startScreenShare,
    stopScreenShare,
    localScreenStream,
    replaceLocalTracks,
  }
}

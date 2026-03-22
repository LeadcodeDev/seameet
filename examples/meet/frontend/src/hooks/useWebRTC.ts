import { useRef, useState, useEffect, useCallback } from 'react'
import type { SignalingMessage } from '@/types'
import type { UseSignalingReturn } from '@/hooks/useSignaling'

export interface RemotePeer {
  id: string
  displayName: string
  stream: MediaStream
  audioTransceiver: RTCRtpTransceiver
  videoTransceiver: RTCRtpTransceiver
}

export interface UseWebRTCOptions {
  participantId: string
  roomId: string
  localStream: MediaStream | null
  signaling: UseSignalingReturn
}

export interface UseWebRTCReturn {
  remotePeers: Map<string, RemotePeer>
  connectionState: RTCPeerConnectionState
  handleMessage: (msg: SignalingMessage) => void
}

export function useWebRTC({
  participantId,
  roomId,
  localStream,
  signaling,
}: UseWebRTCOptions): UseWebRTCReturn {
  const pcRef = useRef<RTCPeerConnection | null>(null)
  const remotePeersRef = useRef<Map<string, RemotePeer>>(new Map())
  const [remotePeers, setRemotePeers] = useState<Map<string, RemotePeer>>(new Map())
  const [connectionState, setConnectionState] = useState<RTCPeerConnectionState>('new')

  const renegotiatingRef = useRef(false)
  const renegotiationPendingRef = useRef(false)
  const renegotiationTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const disconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)

  // Message queue to serialize async message processing (like browser-demo's await)
  const messageQueueRef = useRef<SignalingMessage[]>([])
  const processingRef = useRef(false)

  // Keep refs for values needed in callbacks to avoid stale closures
  const participantIdRef = useRef(participantId)
  const roomIdRef = useRef(roomId)
  const localStreamRef = useRef(localStream)
  const signalingRef = useRef(signaling)

  participantIdRef.current = participantId
  roomIdRef.current = roomId
  localStreamRef.current = localStream
  signalingRef.current = signaling

  const updateRemotePeersState = useCallback(() => {
    setRemotePeers(new Map(remotePeersRef.current))
  }, [])

  const addRemotePeer = useCallback((peerId: string, displayName?: string) => {
    const pc = pcRef.current
    if (!pc || remotePeersRef.current.has(peerId)) return

    const audioTransceiver = pc.addTransceiver('audio', { direction: 'sendrecv' })
    const videoTransceiver = pc.addTransceiver('video', { direction: 'sendrecv' })
    const stream = new MediaStream()

    const peer: RemotePeer = {
      id: peerId,
      displayName: displayName ?? peerId.slice(0, 8),
      stream,
      audioTransceiver,
      videoTransceiver,
    }

    remotePeersRef.current.set(peerId, peer)
    updateRemotePeersState()
    console.log(`[WebRTC] addRemotePeer: ${peerId.slice(0, 8)}, total: ${remotePeersRef.current.size}`)
  }, [updateRemotePeersState])

  const removeRemotePeer = useCallback((peerId: string) => {
    const info = remotePeersRef.current.get(peerId)
    if (!info) return

    try { info.audioTransceiver.direction = 'inactive' } catch { /* ignore */ }
    try { info.videoTransceiver.direction = 'inactive' } catch { /* ignore */ }

    remotePeersRef.current.delete(peerId)
    updateRemotePeersState()
    console.log(`[WebRTC] removeRemotePeer: ${peerId.slice(0, 8)}`)
  }, [updateRemotePeersState])

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

    const pc = new RTCPeerConnection({ iceServers: [] })
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
      console.log(`[WebRTC] ontrack: ${evt.track.kind} (mid=${evt.transceiver.mid})`)
      for (const [peerId, info] of remotePeersRef.current) {
        if (evt.transceiver === info.audioTransceiver ||
            evt.transceiver === info.videoTransceiver) {
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
      console.log(`[WebRTC] unmatched track (mid=${evt.transceiver.mid})`)
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

    // Add local tracks (must be available — CallContext waits for localStream)
    const stream = localStreamRef.current
    if (stream) {
      stream.getTracks().forEach((track) => {
        pc.addTrack(track, stream)
      })
      console.log(`[WebRTC] added ${stream.getTracks().length} local tracks`)
    } else {
      pc.addTransceiver('audio', { direction: 'sendrecv' })
      pc.addTransceiver('video', { direction: 'sendrecv' })
      console.log('[WebRTC] no local media — added dummy transceivers')
    }

    // Add transceivers for each existing peer
    for (const peerId of existingPeers) {
      addRemotePeer(peerId, displayNames?.[peerId])
    }

    // Create and send offer
    const offer = await pc.createOffer()
    await pc.setLocalDescription(offer)

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

    if (data.type === 'peer_joined') {
      const peerId = data.participant
      console.log(`[WebRTC] peer_joined: ${peerId.slice(0, 8)}`)
      const pc = pcRef.current
      if (pc && !remotePeersRef.current.has(peerId)) {
        addRemotePeer(peerId, data.display_name)
        await renegotiate()
      }
      return
    }

    if (data.type === 'peer_left') {
      console.log(`[WebRTC] peer_left: ${data.participant.slice(0, 8)}`)
      removeRemotePeer(data.participant)
      return
    }
  }, [createOfferToServer, addRemotePeer, removeRemotePeer, renegotiate])

  // Process message queue sequentially (like browser-demo's await handleSignalingMessage)
  const drainQueue = useCallback(async () => {
    if (processingRef.current) return
    processingRef.current = true

    while (messageQueueRef.current.length > 0) {
      const msg = messageQueueRef.current.shift()!
      await processMessage(msg)
    }

    processingRef.current = false
  }, [processMessage])

  // Public handler: enqueue message and drain
  const handleMessage = useCallback((msg: SignalingMessage) => {
    messageQueueRef.current.push(msg)
    drainQueue()
  }, [drainQueue])

  // Cleanup on unmount
  useEffect(() => {
    return () => {
      if (renegotiationTimerRef.current) {
        clearTimeout(renegotiationTimerRef.current)
      }
      if (disconnectTimerRef.current) {
        clearTimeout(disconnectTimerRef.current)
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
  }
}

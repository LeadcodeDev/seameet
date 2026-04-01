import { useRef, useCallback, useState, useEffect } from 'react'
import type { SignalingMessage } from '@/types'

export interface UseSignalingOptions {
  url?: string
  onMessage: (msg: SignalingMessage) => void
}

export interface UseSignalingReturn {
  send: (msg: SignalingMessage) => void
  state: 'connecting' | 'open' | 'closed'
  close: () => void
  join: (participantId: string, roomId: string, displayName?: string) => void
  sendOffer: (from: string, roomId: string, sdp: string) => void
  sendAnswer: (from: string, to: string, roomId: string, sdp: string) => void
  sendIceCandidate: (from: string, to: string, roomId: string, candidate: RTCIceCandidate) => void
  sendMuteAudio: (from: string, roomId: string) => void
  sendUnmuteAudio: (from: string, roomId: string) => void
  sendVideoConfig: (from: string, roomId: string, width: number, height: number, fps: number) => void
  sendChatMessage: (from: string, roomId: string, content: string, displayName?: string) => void
}

const DEFAULT_WS_URL = import.meta.env.VITE_WS_URL ?? `ws://${window.location.hostname}:3001`

export function useSignaling({ url, onMessage }: UseSignalingOptions): UseSignalingReturn {
  const wsUrl = url ?? DEFAULT_WS_URL
  const wsRef = useRef<WebSocket | null>(null)
  const onMessageRef = useRef(onMessage)
  const reconnectDelayRef = useRef(1000)
  const reconnectTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const mountedRef = useRef(true)
  const [state, setState] = useState<'connecting' | 'open' | 'closed'>('connecting')

  // Keep onMessage ref fresh to avoid stale closures
  onMessageRef.current = onMessage

  const connect = useCallback(() => {
    if (!mountedRef.current) return

    setState('connecting')
    const ws = new WebSocket(wsUrl)
    wsRef.current = ws

    ws.onopen = () => {
      if (!mountedRef.current) { ws.close(); return }
      if (wsRef.current !== ws) { ws.close(); return }
      setState('open')
      reconnectDelayRef.current = 1000
    }

    ws.onmessage = (evt) => {
      try {
        const data = JSON.parse(evt.data) as SignalingMessage
        onMessageRef.current(data)
      } catch {
        // ignore malformed messages
      }
    }

    ws.onclose = () => {
      if (!mountedRef.current) return
      // Ignore onclose from a stale WebSocket (e.g. React StrictMode cleanup
      // closed WS1 but mount 2 already created WS2 — WS1's async onclose
      // must NOT trigger a reconnect that would overwrite wsRef).
      if (wsRef.current !== ws) return
      setState('closed')
      // Auto-reconnect with exponential backoff
      const delay = reconnectDelayRef.current
      reconnectDelayRef.current = Math.min(delay * 2, 10000)
      reconnectTimerRef.current = setTimeout(connect, delay)
    }

    ws.onerror = () => {
      // onclose will fire after onerror
    }
  }, [wsUrl])

  useEffect(() => {
    mountedRef.current = true
    connect()

    return () => {
      mountedRef.current = false
      if (reconnectTimerRef.current) {
        clearTimeout(reconnectTimerRef.current)
        reconnectTimerRef.current = null
      }
      if (wsRef.current) {
        wsRef.current.close()
        wsRef.current = null
      }
    }
  }, [connect])

  const send = useCallback((msg: SignalingMessage) => {
    if (wsRef.current && wsRef.current.readyState === WebSocket.OPEN) {
      wsRef.current.send(JSON.stringify(msg))
    }
  }, [])

  const join = useCallback((participantId: string, roomId: string, displayName?: string) => {
    send({
      type: 'join',
      participant: participantId,
      room_id: roomId,
      display_name: displayName,
    })
  }, [send])

  const sendOffer = useCallback((from: string, roomId: string, sdp: string) => {
    send({
      type: 'offer',
      from,
      to: null,
      room_id: roomId,
      sdp,
    })
  }, [send])

  const sendAnswer = useCallback((from: string, to: string, roomId: string, sdp: string) => {
    send({
      type: 'answer',
      from,
      to,
      room_id: roomId,
      sdp,
    })
  }, [send])

  const sendIceCandidate = useCallback((from: string, to: string, roomId: string, candidate: RTCIceCandidate) => {
    send({
      type: 'ice_candidate',
      from,
      to,
      room_id: roomId,
      candidate: candidate.candidate,
      sdp_mid: candidate.sdpMid ?? null,
      sdp_mline_index: candidate.sdpMLineIndex ?? null,
    })
  }, [send])

  const sendMuteAudio = useCallback((from: string, roomId: string) => {
    send({ type: 'mute_audio', from, room_id: roomId })
  }, [send])

  const sendUnmuteAudio = useCallback((from: string, roomId: string) => {
    send({ type: 'unmute_audio', from, room_id: roomId })
  }, [send])

  const sendVideoConfig = useCallback((from: string, roomId: string, width: number, height: number, fps: number) => {
    send({ type: 'video_config_changed', from, room_id: roomId, width, height, fps })
  }, [send])

  const sendChatMessage = useCallback((from: string, roomId: string, content: string, displayName?: string) => {
    send({ type: 'chat_message', from, room_id: roomId, content, display_name: displayName, timestamp: Date.now() })
  }, [send])

  const close = useCallback(() => {
    mountedRef.current = false
    if (reconnectTimerRef.current) {
      clearTimeout(reconnectTimerRef.current)
      reconnectTimerRef.current = null
    }
    if (wsRef.current) {
      wsRef.current.close()
      wsRef.current = null
    }
  }, [])

  return { send, state, close, join, sendOffer, sendAnswer, sendIceCandidate, sendMuteAudio, sendUnmuteAudio, sendVideoConfig, sendChatMessage }
}

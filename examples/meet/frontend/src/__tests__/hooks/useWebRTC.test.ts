import { describe, it, expect, vi } from 'vitest'
import { renderHook, act } from '@testing-library/react'
import { useWebRTC } from '@/hooks/useWebRTC'
import type { UseSignalingReturn } from '@/hooks/useSignaling'
import type { SignalingMessage } from '@/types'
import { createMockStream } from '../mocks/mock-media'
import { MockRTCPeerConnection } from '../mocks/mock-rtc'

function createMockSignaling(): UseSignalingReturn & { _sent: SignalingMessage[] } {
  const sent: SignalingMessage[] = []
  return {
    _sent: sent,
    state: 'open',
    send: vi.fn((msg: SignalingMessage) => sent.push(msg)),
    join: vi.fn(),
    sendOffer: vi.fn((from, roomId, sdp) => sent.push({ type: 'offer', from, to: null, room_id: roomId, sdp })),
    sendAnswer: vi.fn(),
    sendIceCandidate: vi.fn(),
    sendMuteAudio: vi.fn(),
    sendUnmuteAudio: vi.fn(),
    sendVideoConfig: vi.fn(),
  }
}

function renderWebRTC(signaling?: ReturnType<typeof createMockSignaling>) {
  const sig = signaling ?? createMockSignaling()
  const localStream = createMockStream(['audio', 'video']) as unknown as MediaStream
  return {
    signaling: sig,
    ...renderHook(() =>
      useWebRTC({
        participantId: 'p1',
        roomId: 'room-1',
        localStream,
        signaling: sig,
        videoSettings: { width: 640, height: 480, frameRate: 24 },
      })
    ),
  }
}

async function flushAsync(): Promise<void> {
  await act(async () => {
    await new Promise(resolve => setTimeout(resolve, 20))
  })
}

describe('useWebRTC', () => {
  it('ready creates PC and sends offer', async () => {
    const { result, signaling } = renderWebRTC()

    await act(async () => {
      result.current.handleMessage({
        type: 'ready',
        room_id: 'room-1',
        initiator: true,
        peers: [],
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    expect(signaling.sendOffer).toHaveBeenCalled()
  })

  it('ready with peers creates entries in remotePeers', async () => {
    const { result } = renderWebRTC()

    await act(async () => {
      result.current.handleMessage({
        type: 'ready',
        room_id: 'room-1',
        initiator: true,
        peers: ['peer-a', 'peer-b'],
        display_names: { 'peer-a': 'Alice', 'peer-b': 'Bob' },
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    expect(result.current.remotePeers.size).toBe(2)
    expect(result.current.remotePeers.get('peer-a')?.displayName).toBe('Alice')
    expect(result.current.remotePeers.get('peer-b')?.displayName).toBe('Bob')
  })

  it('room_status adds new peer to map', async () => {
    const { result } = renderWebRTC()

    // First send ready to initialize PC
    await act(async () => {
      result.current.handleMessage({
        type: 'ready',
        room_id: 'room-1',
        initiator: true,
        peers: [],
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    await act(async () => {
      result.current.handleMessage({
        type: 'room_status',
        room_id: 'room-1',
        participants: [
          { id: 'p1', audio_muted: false, video_muted: false, screen_sharing: false },
          { id: 'peer-c', display_name: 'Charlie', audio_muted: false, video_muted: false, screen_sharing: false },
        ],
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    expect(result.current.remotePeers.has('peer-c')).toBe(true)
    expect(result.current.remotePeers.get('peer-c')?.displayName).toBe('Charlie')
  })

  it('room_status removes peer no longer present', async () => {
    const { result } = renderWebRTC()

    await act(async () => {
      result.current.handleMessage({
        type: 'ready',
        room_id: 'room-1',
        initiator: true,
        peers: ['peer-a'],
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    expect(result.current.remotePeers.has('peer-a')).toBe(true)

    await act(async () => {
      result.current.handleMessage({
        type: 'room_status',
        room_id: 'room-1',
        participants: [
          { id: 'p1', audio_muted: false, video_muted: false, screen_sharing: false },
        ],
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    expect(result.current.remotePeers.has('peer-a')).toBe(false)
  })

  it('room_status sets audioMuted on peer', async () => {
    const { result } = renderWebRTC()

    await act(async () => {
      result.current.handleMessage({
        type: 'ready',
        room_id: 'room-1',
        initiator: true,
        peers: ['peer-a'],
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    await act(async () => {
      result.current.handleMessage({
        type: 'room_status',
        room_id: 'room-1',
        participants: [
          { id: 'p1', audio_muted: false, video_muted: false, screen_sharing: false },
          { id: 'peer-a', audio_muted: true, video_muted: false, screen_sharing: false },
        ],
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    expect(result.current.remotePeers.get('peer-a')?.audioMuted).toBe(true)
  })

  it('room_status clears audioMuted on peer', async () => {
    const { result } = renderWebRTC()

    await act(async () => {
      result.current.handleMessage({
        type: 'ready',
        room_id: 'room-1',
        initiator: true,
        peers: ['peer-a'],
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    // Mute first
    await act(async () => {
      result.current.handleMessage({
        type: 'room_status',
        room_id: 'room-1',
        participants: [
          { id: 'p1', audio_muted: false, video_muted: false, screen_sharing: false },
          { id: 'peer-a', audio_muted: true, video_muted: false, screen_sharing: false },
        ],
      })
      await new Promise(resolve => setTimeout(resolve, 10))
    })
    expect(result.current.remotePeers.get('peer-a')?.audioMuted).toBe(true)

    // Then unmute
    await act(async () => {
      result.current.handleMessage({
        type: 'room_status',
        room_id: 'room-1',
        participants: [
          { id: 'p1', audio_muted: false, video_muted: false, screen_sharing: false },
          { id: 'peer-a', audio_muted: false, video_muted: false, screen_sharing: false },
        ],
      })
      await new Promise(resolve => setTimeout(resolve, 10))
    })
    expect(result.current.remotePeers.get('peer-a')?.audioMuted).toBe(false)
  })

  it('room_status toggles videoMuted', async () => {
    const { result } = renderWebRTC()

    await act(async () => {
      result.current.handleMessage({
        type: 'ready',
        room_id: 'room-1',
        initiator: true,
        peers: ['peer-a'],
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    await act(async () => {
      result.current.handleMessage({
        type: 'room_status',
        room_id: 'room-1',
        participants: [
          { id: 'p1', audio_muted: false, video_muted: false, screen_sharing: false },
          { id: 'peer-a', audio_muted: false, video_muted: true, screen_sharing: false },
        ],
      })
      await new Promise(resolve => setTimeout(resolve, 10))
    })
    expect(result.current.remotePeers.get('peer-a')?.videoMuted).toBe(true)

    await act(async () => {
      result.current.handleMessage({
        type: 'room_status',
        room_id: 'room-1',
        participants: [
          { id: 'p1', audio_muted: false, video_muted: false, screen_sharing: false },
          { id: 'peer-a', audio_muted: false, video_muted: false, screen_sharing: false },
        ],
      })
      await new Promise(resolve => setTimeout(resolve, 10))
    })
    expect(result.current.remotePeers.get('peer-a')?.videoMuted).toBe(false)
  })

  it('screen_share_started sets screenStream on peer', async () => {
    const { result } = renderWebRTC()

    await act(async () => {
      result.current.handleMessage({
        type: 'ready',
        room_id: 'room-1',
        initiator: true,
        peers: ['peer-a'],
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    // Provide an answer so renegotiation completes
    await act(async () => {
      result.current.handleMessage({
        type: 'answer',
        from: 'server',
        to: 'p1',
        room_id: 'room-1',
        sdp: 'mock-answer-sdp',
      })
      await new Promise(resolve => setTimeout(resolve, 10))
    })

    await act(async () => {
      result.current.handleMessage({
        type: 'screen_share_started',
        from: 'peer-a',
        room_id: 'room-1',
        track_id: 0,
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    const peer = result.current.remotePeers.get('peer-a')
    expect(peer?.screenStream).not.toBeNull()
    expect(peer?.screenTransceiver).not.toBeNull()
  })

  it('screen_share_stopped clears screenStream on peer', async () => {
    const { result } = renderWebRTC()

    await act(async () => {
      result.current.handleMessage({
        type: 'ready',
        room_id: 'room-1',
        initiator: true,
        peers: ['peer-a'],
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    // Answer
    await act(async () => {
      result.current.handleMessage({
        type: 'answer', from: 'server', to: 'p1', room_id: 'room-1', sdp: 'mock-answer-sdp',
      })
      await new Promise(resolve => setTimeout(resolve, 10))
    })

    // Start screen share
    await act(async () => {
      result.current.handleMessage({
        type: 'screen_share_started', from: 'peer-a', room_id: 'room-1', track_id: 0,
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    // Answer renegotiation
    await act(async () => {
      result.current.handleMessage({
        type: 'answer', from: 'server', to: 'p1', room_id: 'room-1', sdp: 'mock-answer-sdp-2',
      })
      await new Promise(resolve => setTimeout(resolve, 10))
    })

    // Stop screen share
    await act(async () => {
      result.current.handleMessage({
        type: 'screen_share_stopped', from: 'peer-a', room_id: 'room-1', track_id: 0,
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    const peer = result.current.remotePeers.get('peer-a')
    expect(peer?.screenStream).toBeNull()
    expect(peer?.screenTransceiver).toBeNull()
  })

  it('recovers a queued renegotiation after setRemoteDescription fails (B2/INV-5)', async () => {
    const { result, signaling } = renderWebRTC()

    await act(async () => {
      result.current.handleMessage({ type: 'ready', room_id: 'room-1', initiator: true, peers: [] })
      await new Promise(r => setTimeout(r, 20))
    })

    // Queue a second renegotiation while the initial offer is still awaiting an answer.
    await act(async () => {
      result.current.handleMessage({ type: 'request_renegotiation', room_id: 'room-1', needed_slots: 2 })
      await new Promise(r => setTimeout(r, 10))
    })

    const offersBefore = signaling.sendOffer.mock.calls.length

    // The next answer fails to apply.
    MockRTCPeerConnection.failNextSetRemoteDescription = true
    await act(async () => {
      result.current.handleMessage({ type: 'answer', from: 'server', to: 'p1', room_id: 'room-1', sdp: 'bad' })
      await new Promise(r => setTimeout(r, 20))
    })

    // The failure path must release the lock AND drain the queued renegotiation → a new offer is sent.
    expect(signaling.sendOffer.mock.calls.length).toBeGreaterThan(offersBefore)
  })

  it('request_renegotiation adds transceiver slots and renegotiates', async () => {
    const { result, signaling } = renderWebRTC()

    await act(async () => {
      result.current.handleMessage({
        type: 'ready',
        room_id: 'room-1',
        initiator: true,
        peers: [],
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    // Answer initial offer
    await act(async () => {
      result.current.handleMessage({
        type: 'answer', from: 'server', to: 'p1', room_id: 'room-1', sdp: 'mock-answer',
      })
      await new Promise(resolve => setTimeout(resolve, 10))
    })

    const offerCountBefore = signaling.sendOffer.mock.calls.length

    await act(async () => {
      result.current.handleMessage({
        type: 'request_renegotiation',
        room_id: 'room-1',
        needed_slots: 3,
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    // Should have sent another offer for renegotiation
    expect(signaling.sendOffer.mock.calls.length).toBeGreaterThan(offerCountBefore)
  })

  it('reconcile is idempotent — repeated identical room_status keeps peers stable (INV-1)', async () => {
    const { result } = renderWebRTC()
    await act(async () => {
      result.current.handleMessage({ type: 'ready', room_id: 'room-1', initiator: true, peers: ['peer-a', 'peer-b'] })
      await new Promise(r => setTimeout(r, 20))
    })

    const status = {
      type: 'room_status' as const, room_id: 'room-1',
      participants: [
        { id: 'p1', audio_muted: false, video_muted: false, screen_sharing: false },
        { id: 'peer-a', audio_muted: false, video_muted: false, screen_sharing: false },
        { id: 'peer-b', audio_muted: false, video_muted: false, screen_sharing: false },
      ],
    }

    await act(async () => { result.current.handleMessage(status); await new Promise(r => setTimeout(r, 10)) })
    const aRef = result.current.remotePeers.get('peer-a')
    await act(async () => { result.current.handleMessage(status); await new Promise(r => setTimeout(r, 10)) })

    expect(result.current.remotePeers.size).toBe(2)
    expect(result.current.remotePeers.get('peer-a')).toBe(aRef)
  })

  it('toggling one peer does not mutate another peer (INV-4)', async () => {
    const { result } = renderWebRTC()
    await act(async () => {
      result.current.handleMessage({ type: 'ready', room_id: 'room-1', initiator: true, peers: ['peer-a', 'peer-b'] })
      await new Promise(r => setTimeout(r, 20))
    })

    await act(async () => {
      result.current.handleMessage({
        type: 'room_status', room_id: 'room-1',
        participants: [
          { id: 'p1', audio_muted: false, video_muted: false, screen_sharing: false },
          { id: 'peer-a', audio_muted: false, video_muted: true, screen_sharing: false },
          { id: 'peer-b', audio_muted: false, video_muted: false, screen_sharing: false },
        ],
      })
      await new Promise(r => setTimeout(r, 10))
    })

    expect(result.current.remotePeers.get('peer-a')?.videoMuted).toBe(true)
    expect(result.current.remotePeers.get('peer-b')?.videoMuted).toBe(false)
    expect(result.current.remotePeers.get('peer-b')?.audioMuted).toBe(false)
  })
})

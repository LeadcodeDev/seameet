import { describe, it, expect, vi } from 'vitest'
import { renderHook, act } from '@testing-library/react'
import { useWebRTC, clampToBwe } from '@/hooks/useWebRTC'
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

  it('screen_share_routed binds the screen transceiver to the server-given mid', async () => {
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

    // Pick a concrete free video transceiver mid from the live pc:
    // one that is NOT peer-a's audioMid/videoMid and has no sender.track.
    const pc = MockRTCPeerConnection.instances.at(-1)!
    const peerA = result.current.remotePeers.get('peer-a')!
    const usedMids = new Set([peerA.audioMid, peerA.videoMid])
    const freeVideoTransceiver = pc.getTransceivers().find(
      t => t.mid !== null &&
           !usedMids.has(t.mid) &&
           t.receiver.track.kind === 'video' &&
           t.sender.track === null,
    )!
    const screenMid = freeVideoTransceiver.mid!

    await act(async () => {
      result.current.handleMessage({
        type: 'screen_share_routed',
        from: 'peer-a',
        mid: screenMid,
        room_id: 'room-1',
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    const peer = result.current.remotePeers.get('peer-a')
    expect(peer?.screenTransceiver?.mid).toBe(screenMid)
    expect(peer?.screenStream).not.toBeNull()
  })

  it('screen_share_started alone does not bind screenTransceiver (only screen_share_routed binds)', async () => {
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
    expect(peer?.screenTransceiver).toBeNull()
    expect(peer?.screenStream).toBeNull()
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

    // Bind via screen_share_routed (authoritative mid from server)
    const pc = MockRTCPeerConnection.instances.at(-1)!
    const peerA = result.current.remotePeers.get('peer-a')!
    const usedMids = new Set([peerA.audioMid, peerA.videoMid])
    const freeVideoTransceiver = pc.getTransceivers().find(
      t => t.mid !== null &&
           !usedMids.has(t.mid) &&
           t.receiver.track.kind === 'video' &&
           t.sender.track === null,
    )!
    const screenMid = freeVideoTransceiver.mid!

    await act(async () => {
      result.current.handleMessage({
        type: 'screen_share_routed', from: 'peer-a', mid: screenMid, room_id: 'room-1',
      })
      await new Promise(resolve => setTimeout(resolve, 20))
    })

    expect(result.current.remotePeers.get('peer-a')?.screenTransceiver).not.toBeNull()

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

  it('routes an ontrack track to the peer owning that mid, even if it arrived before re-add (INV-2)', async () => {
    const { result } = renderWebRTC()
    await act(async () => {
      result.current.handleMessage({ type: 'ready', room_id: 'room-1', initiator: true, peers: ['peer-a'] })
      await new Promise(r => setTimeout(r, 20))
    })

    const videoMid = result.current.remotePeers.get('peer-a')!.videoMid!
    const pc = MockRTCPeerConnection.instances.at(-1)!

    // Remove peer-a so its slot (and mid) returns to the pool.
    await act(async () => {
      result.current.handleMessage({
        type: 'room_status', room_id: 'room-1',
        participants: [{ id: 'p1', audio_muted: false, video_muted: false, screen_sharing: false }],
      })
      await new Promise(r => setTimeout(r, 10))
    })

    // A track arrives for that mid while no peer owns it → must be buffered, not dropped.
    const lateTrack = { kind: 'video', id: 'late-video-track' } as unknown as MediaStreamTrack
    await act(async () => {
      pc.ontrack?.({ track: lateTrack, transceiver: { mid: videoMid } } as unknown as RTCTrackEvent)
      await new Promise(r => setTimeout(r, 5))
    })

    // peer-a rejoins, reusing the same front-of-pool slot/mid → buffered track is attached.
    await act(async () => {
      result.current.handleMessage({
        type: 'room_status', room_id: 'room-1',
        participants: [
          { id: 'p1', audio_muted: false, video_muted: false, screen_sharing: false },
          { id: 'peer-a', audio_muted: false, video_muted: false, screen_sharing: false },
        ],
      })
      await new Promise(r => setTimeout(r, 10))
    })

    const stream = result.current.remotePeers.get('peer-a')!.stream
    expect(stream.getTracks().some(t => t.id === 'late-video-track')).toBe(true)
  })

  it('requests a keyframe for each newly added peer', async () => {
    const { result, signaling } = renderWebRTC()
    await act(async () => {
      result.current.handleMessage({ type: 'ready', room_id: 'room-1', initiator: true, peers: ['peer-a'] })
      await new Promise(r => setTimeout(r, 20))
    })
    const kf = signaling._sent.filter(m => m.type === 'request_keyframe' && (m as { target: string }).target === 'peer-a')
    expect(kf.length).toBeGreaterThan(0)
  })

  it('adds peers skipped due to pool exhaustion after request_renegotiation grows the pool (B1)', async () => {
    const { result } = renderWebRTC()

    // Join with no existing peers → pool = MAX_PEER_SLOTS (7).
    await act(async () => {
      result.current.handleMessage({ type: 'ready', room_id: 'room-1', initiator: true, peers: [] })
      await new Promise(r => setTimeout(r, 20))
    })
    // Apply the initial answer so the connection is established.
    await act(async () => {
      result.current.handleMessage({ type: 'answer', from: 'server', to: 'p1', room_id: 'room-1', sdp: 'a0' })
      await new Promise(r => setTimeout(r, 10))
    })

    // room_status with 8 REMOTE peers (+ self) → only 7 fit; the 8th is skipped.
    const participants = [
      { id: 'p1', audio_muted: false, video_muted: false, screen_sharing: false },
      ...Array.from({ length: 8 }, (_, i) => ({
        id: `peer-${i}`, audio_muted: false, video_muted: false, screen_sharing: false,
      })),
    ]
    await act(async () => {
      result.current.handleMessage({ type: 'room_status', room_id: 'room-1', participants })
      await new Promise(r => setTimeout(r, 20))
    })
    expect(result.current.remotePeers.size).toBe(7) // 8th skipped — pool exhausted

    // Server asks for 1 more slot. The handler grows the pool, renegotiates,
    // and (the fix) re-reconciles → the 8th peer is finally added.
    await act(async () => {
      result.current.handleMessage({ type: 'request_renegotiation', room_id: 'room-1', needed_slots: 1 })
      await new Promise(r => setTimeout(r, 20))
    })
    // Apply the renegotiation answer.
    await act(async () => {
      result.current.handleMessage({ type: 'answer', from: 'server', to: 'p1', room_id: 'room-1', sdp: 'a1' })
      await new Promise(r => setTimeout(r, 20))
    })

    expect(result.current.remotePeers.size).toBe(8)
  })

  it('buffers ICE candidates that arrive before the answer, then flushes them (A4)', async () => {
    const { result } = renderWebRTC()
    await act(async () => {
      result.current.handleMessage({ type: 'ready', room_id: 'room-1', initiator: true, peers: [] })
      await new Promise(r => setTimeout(r, 20))
    })

    const pc = MockRTCPeerConnection.instances.at(-1)!
    const addSpy = vi.spyOn(pc, 'addIceCandidate')

    // Candidate arrives before any answer → remoteDescription is null → must be buffered.
    await act(async () => {
      result.current.handleMessage({
        type: 'ice_candidate', from: 'server', to: 'p1', room_id: 'room-1',
        candidate: 'candidate:1 1 udp 2122260223 192.168.1.2 54321 typ host',
        sdp_mid: '0', sdp_mline_index: 0,
      })
      await new Promise(r => setTimeout(r, 10))
    })
    expect(addSpy).not.toHaveBeenCalled()

    // Answer applied → remoteDescription set → buffered candidate is flushed.
    await act(async () => {
      result.current.handleMessage({ type: 'answer', from: 'server', to: 'p1', room_id: 'room-1', sdp: 'mock-answer' })
      await new Promise(r => setTimeout(r, 10))
    })
    expect(addSpy).toHaveBeenCalledTimes(1)
  })
})

describe('clampToBwe', () => {
  it('returns base when no BWE cap', () => {
    expect(clampToBwe(800_000, null)).toBe(800_000)
  })
  it('clamps to the BWE cap when it is lower', () => {
    expect(clampToBwe(800_000, 200_000)).toBe(200_000)
  })
  it('keeps base when it is already below the BWE cap', () => {
    expect(clampToBwe(100_000, 200_000)).toBe(100_000)
  })
})

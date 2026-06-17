import { describe, it, expect, vi, afterEach } from 'vitest'
import { render, cleanup } from '@testing-library/react'
import { VideoGrid } from '@/components/VideoGrid'
import { createMockStream } from '../mocks/mock-media'
import React from 'react'
import { VIDEO_GRID_BUDGET } from '@/lib/roomMode'

afterEach(cleanup)

// ── Mock useCall ────────────────────────────────────────────────────────

interface MockCallValues {
  localStream: MediaStream | null
  remotePeers: Map<string, {
    id: string
    displayName: string
    stream: MediaStream | null
    screenStream: MediaStream | null
    audioMuted: boolean
    videoMuted: boolean
    e2ee?: boolean
  }>
  displayName: string
  audioEnabled: boolean
  videoEnabled: boolean
  localScreenStream: MediaStream | null
  e2eeEnabled: boolean
  e2eePeerStates: Map<string, { ready: boolean }>
  e2eeNotReady: { local: boolean; peers: Set<string> }
  activeSpeakerId: string | null
  recentSpeakers: string[]
  participantId: string
  verificationStatus: (peerId: string) => 'unverified' | 'verified' | 'changed'
}

let mockCallValues: MockCallValues

vi.mock('@/context/CallContext', () => ({
  useCall: () => mockCallValues,
}))

function makePeer(id: string, displayName: string, opts?: Partial<MockCallValues['remotePeers'] extends Map<string, infer V> ? V : never>) {
  return {
    id,
    displayName,
    stream: null,
    screenStream: null,
    audioMuted: false,
    videoMuted: false,
    ...opts,
  }
}

function setCallValues(overrides: Partial<MockCallValues> = {}) {
  mockCallValues = {
    localStream: null,
    remotePeers: new Map(),
    displayName: 'Local User',
    audioEnabled: true,
    videoEnabled: true,
    localScreenStream: null,
    e2eeEnabled: false,
    e2eePeerStates: new Map(),
    e2eeNotReady: { local: false, peers: new Set() },
    activeSpeakerId: null,
    recentSpeakers: [],
    participantId: 'local-id',
    verificationStatus: () => 'unverified',
    ...overrides,
  }
}

function makeMany(n: number): Map<string, ReturnType<typeof makePeer>> {
  const m = new Map<string, ReturnType<typeof makePeer>>()
  for (let i = 0; i < n; i++) {
    m.set(`peer-${i}`, makePeer(`peer-${i}`, `User ${i}`))
  }
  return m
}

describe('VideoGrid — uniform grid', () => {
  it('always renders a single video-grid container (no spotlight, no filmstrip)', () => {
    setCallValues({
      remotePeers: new Map([
        ['p1', makePeer('p1', 'A')],
        ['p2', makePeer('p2', 'B')],
        ['p3', makePeer('p3', 'C')],
        ['p4', makePeer('p4', 'D')],
      ]),
      activeSpeakerId: 'p1',
    })
    const { container } = render(<VideoGrid />)

    const grids = container.querySelectorAll('[data-testid="video-grid"]')
    expect(grids.length).toBe(1)
    // Must be a CSS grid, not a flex filmstrip
    expect(grids[0].className).toContain('grid')
  })

  it('renders the local tile plus one tile per remote peer', () => {
    setCallValues({
      remotePeers: new Map([
        ['p1', makePeer('p1', 'Bob')],
        ['p2', makePeer('p2', 'Carol')],
      ]),
    })
    const { container } = render(<VideoGrid />)
    const tiles = container.querySelectorAll('[data-testid="video-tile"]')
    // local + 2 peers
    expect(tiles.length).toBe(3)
  })

  it('renders screen share tiles inside the same grid', () => {
    setCallValues({
      remotePeers: new Map([
        ['p1', makePeer('p1', 'Bob', { screenStream: createMockStream(['video']) as unknown as MediaStream })],
      ]),
    })
    const { container } = render(<VideoGrid />)

    const tiles = container.querySelectorAll('[data-testid="video-tile"]')
    // local + remote camera + remote screen share = 3
    expect(tiles.length).toBe(3)

    // All tiles are children of the same grid
    const grid = container.querySelector('[data-testid="video-grid"]')!
    expect(grid).toBeTruthy()
    // No separate filmstrip or avatar-grid element
    expect(container.querySelector('[data-testid="avatar-grid"]')).toBeNull()
  })

  it('renders local screen share tile inside the same grid', () => {
    setCallValues({ localScreenStream: createMockStream(['video']) as unknown as MediaStream })
    const { container } = render(<VideoGrid />)

    const tiles = container.querySelectorAll('[data-testid="video-tile"]')
    // local + local screen share = 2
    expect(tiles.length).toBe(2)
  })

  it('with ≤ VIDEO_GRID_BUDGET total tiles, every peer tile reflects its own mute state', () => {
    // 1 local + 3 peers = 4 total (well within budget=10)
    const peers = new Map([
      ['p1', makePeer('p1', 'A', { videoMuted: false })],
      ['p2', makePeer('p2', 'B', { videoMuted: true })],
      ['p3', makePeer('p3', 'C', { videoMuted: false })],
    ])
    setCallValues({ remotePeers: peers })
    const { container } = render(<VideoGrid />)

    const tileByName = (name: string) =>
      container.querySelector(`[data-participant="${name}"]`)!

    expect(tileByName('A').getAttribute('data-video')).toBe('on')
    expect(tileByName('B').getAttribute('data-video')).toBe('off')
    expect(tileByName('C').getAttribute('data-video')).toBe('on')
  })

  it('with > VIDEO_GRID_BUDGET total tiles, only budget peers show video; rest show avatar', () => {
    // 1 local + 14 peers = 15 total > budget(10)
    // peer-0 is active speaker, peer-1 peer-2 are recent speakers
    // They should be in the visible video set; peer-3 through peer-13 fill rest up to budget
    // Budget is 10, so active + 2 recent + 7 more = 10 video peers
    const n = 14
    setCallValues({
      remotePeers: makeMany(n),
      activeSpeakerId: 'peer-0',
      recentSpeakers: ['peer-0', 'peer-1', 'peer-2'],
    })
    const { container } = render(<VideoGrid />)

    const allTiles = container.querySelectorAll('[data-testid="video-tile"]')
    // 1 local + 14 peers (no screen shares)
    expect(allTiles.length).toBe(15)

    const videoOnTiles = container.querySelectorAll('[data-video="on"]')
    const videoOffTiles = container.querySelectorAll('[data-video="off"]')

    // Local tile is always on (videoEnabled=true in defaults)
    // Budget peers are on, rest are off
    // Total "on" = 1 (local) + VIDEO_GRID_BUDGET peers = 11
    expect(videoOnTiles.length).toBe(1 + VIDEO_GRID_BUDGET)
    expect(videoOffTiles.length).toBe(n - VIDEO_GRID_BUDGET)
  })

  it('with > budget tiles, speakers are in the video set and non-speakers get avatars', () => {
    const n = 15
    // 1 local + 15 peers = 16 > budget(10)
    const remotePeers = makeMany(n)
    setCallValues({
      remotePeers,
      activeSpeakerId: 'peer-14', // last peer (would not be in budget by iteration order)
      recentSpeakers: ['peer-14'],
    })
    const { container } = render(<VideoGrid />)

    // peer-14 must have video on (it's the active speaker)
    const speakerTile = container.querySelector('[data-participant="User 14"]')!
    expect(speakerTile).toBeTruthy()
    expect(speakerTile.getAttribute('data-video')).toBe('on')

    // peer-10 through peer-13 should be off (beyond budget after speaker+9 others)
    // Actually: active speaker peer-14 is slot 1; then recent speakers (already in set);
    // then peer-0 to peer-8 fill slots 2-10. peer-9 through peer-13 (except peer-14) are off.
    // peer-9 is beyond budget (10 slots used: peer-14 + peer-0..peer-8)
    const peer9Tile = container.querySelector('[data-participant="User 9"]')!
    expect(peer9Tile.getAttribute('data-video')).toBe('off')
  })

  it('no avatar-tile elements are rendered — budget peers use VideoTile with videoEnabled=false', () => {
    setCallValues({
      remotePeers: makeMany(20),
      activeSpeakerId: 'peer-0',
      recentSpeakers: [],
    })
    const { container } = render(<VideoGrid />)

    // AvatarTile has data-testid="avatar-tile"; it must not exist
    expect(container.querySelector('[data-testid="avatar-tile"]')).toBeNull()

    // But avatar-placeholders inside VideoTile exist for the non-budget peers
    const avatarPlaceholders = container.querySelectorAll('[data-testid="avatar-placeholder"]')
    expect(avatarPlaceholders.length).toBeGreaterThan(0)
  })
})

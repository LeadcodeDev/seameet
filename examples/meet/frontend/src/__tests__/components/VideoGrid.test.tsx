import { describe, it, expect, vi, afterEach } from 'vitest'
import { render, cleanup } from '@testing-library/react'
import { VideoGrid } from '@/components/VideoGrid'
import { createMockStream } from '../mocks/mock-media'
import React from 'react'

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
  activeSpeakerId: string | null
  participantId: string
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
    activeSpeakerId: null,
    participantId: 'local-id',
    ...overrides,
  }
}

describe('VideoGrid', () => {
  it('1 participant renders grid-cols-1', () => {
    setCallValues()
    const { container } = render(<VideoGrid />)
    const grid = container.querySelector('[data-testid="video-grid"]')!
    expect(grid.className).toContain('grid-cols-1')
  })

  it('2 participants renders grid-cols-2', () => {
    setCallValues({
      remotePeers: new Map([['peer-1', makePeer('peer-1', 'Bob')]]),
    })
    const { container } = render(<VideoGrid />)
    const grid = container.querySelector('[data-testid="video-grid"]')!
    expect(grid.className).toContain('grid-cols-2')
  })

  it('5+ tiles with active speaker uses spotlight layout', () => {
    const peers = new Map([
      ['p1', makePeer('p1', 'A')],
      ['p2', makePeer('p2', 'B')],
      ['p3', makePeer('p3', 'C')],
      ['p4', makePeer('p4', 'D')],
    ])
    setCallValues({ remotePeers: peers, activeSpeakerId: 'p1' })
    const { container } = render(<VideoGrid />)

    const grid = container.querySelector('[data-testid="video-grid"]')!
    expect(grid.className).toContain('flex')
    expect(grid.className).not.toContain('grid-cols')
  })

  it('5+ tiles without active speaker uses grid layout', () => {
    const peers = new Map([
      ['p1', makePeer('p1', 'A')],
      ['p2', makePeer('p2', 'B')],
      ['p3', makePeer('p3', 'C')],
      ['p4', makePeer('p4', 'D')],
    ])
    setCallValues({ remotePeers: peers, activeSpeakerId: null })
    const { container } = render(<VideoGrid />)

    const grid = container.querySelector('[data-testid="video-grid"]')!
    expect(grid.className).toContain('grid')
  })

  it('renders screen share tiles separately', () => {
    setCallValues({
      remotePeers: new Map([
        ['p1', makePeer('p1', 'Bob', { screenStream: createMockStream(['video']) as unknown as MediaStream })],
      ]),
    })
    const { container } = render(<VideoGrid />)

    const tiles = container.querySelectorAll('[data-testid="video-tile"]')
    // local + remote + remote's screen share = 3
    expect(tiles.length).toBe(3)
  })

  it('renders local screen share tile', () => {
    setCallValues({ localScreenStream: createMockStream(['video']) as unknown as MediaStream })
    const { container } = render(<VideoGrid />)

    const tiles = container.querySelectorAll('[data-testid="video-tile"]')
    // local + local screen share = 2
    expect(tiles.length).toBe(2)
  })
})

import { describe, it, expect } from 'vitest'
import {
  pickVisibleVideoSet,
  pushRecentSpeaker,
  VIDEO_GRID_BUDGET,
} from '@/lib/roomMode'

function ids(n: number, prefix = 'p'): string[] {
  return Array.from({ length: n }, (_, i) => `${prefix}-${i + 1}`)
}

describe('pickVisibleVideoSet', () => {
  it('returns all peers when count is within budget', () => {
    const peerIds = ids(VIDEO_GRID_BUDGET - 1)
    const visible = pickVisibleVideoSet({
      peerIds,
      localId: 'me',
      activeSpeakerId: null,
      recentSpeakers: [],
    })
    expect(visible.size).toBe(peerIds.length)
    for (const id of peerIds) expect(visible.has(id)).toBe(true)
  })

  it('caps at budget when peers exceed it', () => {
    const peerIds = ids(VIDEO_GRID_BUDGET + 10)
    const visible = pickVisibleVideoSet({
      peerIds,
      localId: 'me',
      activeSpeakerId: null,
      recentSpeakers: [],
    })
    expect(visible.size).toBe(VIDEO_GRID_BUDGET)
  })

  it('prioritises active speaker first', () => {
    const peerIds = ids(VIDEO_GRID_BUDGET + 5)
    const activeSpeakerId = `p-${VIDEO_GRID_BUDGET + 3}` // near the end
    const visible = pickVisibleVideoSet({
      peerIds,
      localId: 'me',
      activeSpeakerId,
      recentSpeakers: [],
    })
    expect(visible.size).toBe(VIDEO_GRID_BUDGET)
    expect(visible.has(activeSpeakerId)).toBe(true)
  })

  it('prioritises recent speakers after active speaker', () => {
    const peerIds = ids(VIDEO_GRID_BUDGET + 5)
    // Place the recent speakers near the end of the list so they would not be
    // included by iteration order alone.
    const activeSpeakerId = `p-${VIDEO_GRID_BUDGET + 4}`
    const recentSpeakers = [`p-${VIDEO_GRID_BUDGET + 2}`, `p-${VIDEO_GRID_BUDGET + 3}`]
    const visible = pickVisibleVideoSet({
      peerIds,
      localId: 'me',
      activeSpeakerId,
      recentSpeakers,
    })
    expect(visible.size).toBe(VIDEO_GRID_BUDGET)
    expect(visible.has(activeSpeakerId)).toBe(true)
    for (const id of recentSpeakers) expect(visible.has(id)).toBe(true)
  })

  it('fills remaining slots from peerIds in order when speakers are scarce', () => {
    const peerIds = ids(VIDEO_GRID_BUDGET + 5)
    const visible = pickVisibleVideoSet({
      peerIds,
      localId: 'me',
      activeSpeakerId: null,
      recentSpeakers: [],
    })
    expect(visible.size).toBe(VIDEO_GRID_BUDGET)
    // First VIDEO_GRID_BUDGET peers from deterministic iteration order
    expect(visible.has('p-1')).toBe(true)
    expect(visible.has(`p-${VIDEO_GRID_BUDGET}`)).toBe(true)
    expect(visible.has(`p-${VIDEO_GRID_BUDGET + 1}`)).toBe(false)
  })

  it('never includes localId in the visible set', () => {
    const peerIds = [...ids(VIDEO_GRID_BUDGET + 5), 'me']
    const visible = pickVisibleVideoSet({
      peerIds,
      localId: 'me',
      activeSpeakerId: 'me',
      recentSpeakers: ['me'],
    })
    expect(visible.has('me')).toBe(false)
  })

  it('ignores active speaker / recent speakers not in the room', () => {
    const peerIds = ids(VIDEO_GRID_BUDGET + 5)
    const visible = pickVisibleVideoSet({
      peerIds,
      localId: 'me',
      activeSpeakerId: 'ghost',
      recentSpeakers: ['ghost-2'],
    })
    expect(visible.has('ghost')).toBe(false)
    expect(visible.has('ghost-2')).toBe(false)
    expect(visible.size).toBe(VIDEO_GRID_BUDGET)
  })

  it('respects a custom budget override', () => {
    const peerIds = ids(20)
    const visible = pickVisibleVideoSet({
      peerIds,
      localId: 'me',
      activeSpeakerId: null,
      recentSpeakers: [],
      budget: 5,
    })
    expect(visible.size).toBe(5)
  })
})

describe('pushRecentSpeaker', () => {
  it('puts the new speaker first', () => {
    const next = pushRecentSpeaker(['a', 'b'], 'c')
    expect(next).toEqual(['c', 'a', 'b'])
  })

  it('deduplicates: the new speaker bubbles to the front', () => {
    const next = pushRecentSpeaker(['a', 'b', 'c'], 'b')
    expect(next).toEqual(['b', 'a', 'c'])
  })

  it('caps the list length', () => {
    const next = pushRecentSpeaker(['a', 'b', 'c', 'd', 'e'], 'f', 5)
    expect(next).toEqual(['f', 'a', 'b', 'c', 'd'])
  })

  it('does not mutate the input array', () => {
    const original = ['a', 'b']
    pushRecentSpeaker(original, 'c')
    expect(original).toEqual(['a', 'b'])
  })
})

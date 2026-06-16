import { describe, it, expect } from 'vitest'
import {
  classifyRoom,
  pickVisibleVideoSet,
  pushRecentSpeaker,
  LARGE_VIDEO_BUDGET,
  MEETING_MAX,
  WEBINAR_MIN,
} from '@/lib/roomMode'

describe('classifyRoom', () => {
  it('treats small rooms as meeting', () => {
    expect(classifyRoom(1)).toBe('meeting')
    expect(classifyRoom(MEETING_MAX)).toBe('meeting')
  })

  it('treats medium rooms as large', () => {
    expect(classifyRoom(MEETING_MAX + 1)).toBe('large')
    expect(classifyRoom(WEBINAR_MIN)).toBe('large')
  })

  it('treats huge rooms as webinar', () => {
    expect(classifyRoom(WEBINAR_MIN + 1)).toBe('webinar')
    expect(classifyRoom(500)).toBe('webinar')
  })
})

function ids(n: number, prefix = 'p'): string[] {
  return Array.from({ length: n }, (_, i) => `${prefix}-${i + 1}`)
}

describe('pickVisibleVideoSet', () => {
  it('meeting mode keeps everyone visible', () => {
    const peerIds = ids(20)
    const visible = pickVisibleVideoSet({
      peerIds,
      localId: 'me',
      activeSpeakerId: null,
      recentSpeakers: [],
      mode: 'meeting',
    })
    expect(visible.size).toBe(20)
    for (const id of peerIds) expect(visible.has(id)).toBe(true)
  })

  it('webinar mode keeps only the active speaker (and never the local user)', () => {
    const visible = pickVisibleVideoSet({
      peerIds: ids(80),
      localId: 'me',
      activeSpeakerId: 'p-7',
      recentSpeakers: ['p-3', 'p-5'],
      mode: 'webinar',
    })
    expect(visible.size).toBe(1)
    expect(visible.has('p-7')).toBe(true)
    expect(visible.has('me')).toBe(false)
  })

  it('webinar mode with no active speaker shows nobody (avatars only)', () => {
    const visible = pickVisibleVideoSet({
      peerIds: ids(80),
      localId: 'me',
      activeSpeakerId: null,
      recentSpeakers: [],
      mode: 'webinar',
    })
    expect(visible.size).toBe(0)
  })

  it('large mode caps at the budget and prioritises speaker + recent speakers', () => {
    const peerIds = ids(40)
    const visible = pickVisibleVideoSet({
      peerIds,
      localId: 'me',
      activeSpeakerId: 'p-30',
      recentSpeakers: ['p-25', 'p-12'],
      mode: 'large',
    })
    expect(visible.size).toBe(LARGE_VIDEO_BUDGET)
    expect(visible.has('p-30')).toBe(true)
    expect(visible.has('p-25')).toBe(true)
    expect(visible.has('p-12')).toBe(true)
  })

  it('large mode fills remaining slots from peerIds order when speakers are scarce', () => {
    const peerIds = ids(40)
    const visible = pickVisibleVideoSet({
      peerIds,
      localId: 'me',
      activeSpeakerId: null,
      recentSpeakers: [],
      mode: 'large',
    })
    expect(visible.size).toBe(LARGE_VIDEO_BUDGET)
    // First N peers from the deterministic iteration order
    expect(visible.has('p-1')).toBe(true)
    expect(visible.has(`p-${LARGE_VIDEO_BUDGET}`)).toBe(true)
    expect(visible.has(`p-${LARGE_VIDEO_BUDGET + 1}`)).toBe(false)
  })

  it('ignores active speaker / recent speakers that are not actually in the room', () => {
    const peerIds = ids(40)
    const visible = pickVisibleVideoSet({
      peerIds,
      localId: 'me',
      activeSpeakerId: 'ghost',
      recentSpeakers: ['ghost-2'],
      mode: 'large',
    })
    expect(visible.has('ghost')).toBe(false)
    expect(visible.has('ghost-2')).toBe(false)
    expect(visible.size).toBe(LARGE_VIDEO_BUDGET)
  })

  it('never includes localId in the visible set (caller renders local tile separately)', () => {
    const peerIds = [...ids(40), 'me']
    const visible = pickVisibleVideoSet({
      peerIds,
      localId: 'me',
      activeSpeakerId: 'me',
      recentSpeakers: ['me'],
      mode: 'large',
    })
    expect(visible.has('me')).toBe(false)
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

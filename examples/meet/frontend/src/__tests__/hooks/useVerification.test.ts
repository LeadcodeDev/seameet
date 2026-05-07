import { describe, it, expect } from 'vitest'
import { renderHook, act } from '@testing-library/react'
import { useVerification } from '@/hooks/useVerification'

describe('useVerification', () => {
  it('peers default to unverified', () => {
    const sn = new Map<string, string>([['p1', '11111 22222 33333']])
    const { result } = renderHook(() => useVerification(sn))
    expect(result.current.status('p1')).toBe('unverified')
    expect(result.current.statuses.size).toBe(0)
  })

  it('markVerified flips status to verified and snapshots the number', () => {
    const sn = new Map<string, string>([['p1', '11111 22222 33333']])
    const { result } = renderHook(() => useVerification(sn))
    act(() => result.current.markVerified('p1'))
    expect(result.current.status('p1')).toBe('verified')
  })

  it('markVerified is a no-op for peers without a known safety number', () => {
    const sn = new Map<string, string>()
    const { result } = renderHook(() => useVerification(sn))
    act(() => result.current.markVerified('p1'))
    expect(result.current.status('p1')).toBe('unverified')
  })

  it('clear resets a verified peer back to unverified', () => {
    const sn = new Map<string, string>([['p1', '11111']])
    const { result } = renderHook(() => useVerification(sn))
    act(() => result.current.markVerified('p1'))
    expect(result.current.status('p1')).toBe('verified')
    act(() => result.current.clear('p1'))
    expect(result.current.status('p1')).toBe('unverified')
  })

  it('flips a verified peer to "changed" when their safety number changes', () => {
    const sn1 = new Map<string, string>([['p1', '11111']])
    const { result, rerender } = renderHook(({ sn }) => useVerification(sn), {
      initialProps: { sn: sn1 },
    })
    act(() => result.current.markVerified('p1'))
    expect(result.current.status('p1')).toBe('verified')

    const sn2 = new Map<string, string>([['p1', '22222']])
    rerender({ sn: sn2 })
    expect(result.current.status('p1')).toBe('changed')
  })

  it('does not flag "changed" when an unverified peer\'s number rotates', () => {
    const sn1 = new Map<string, string>([['p1', '11111']])
    const { result, rerender } = renderHook(({ sn }) => useVerification(sn), {
      initialProps: { sn: sn1 },
    })
    const sn2 = new Map<string, string>([['p1', '22222']])
    rerender({ sn: sn2 })
    expect(result.current.status('p1')).toBe('unverified')
  })

  it('drops verification when the peer disappears from the safety-number map', () => {
    const sn1 = new Map<string, string>([['p1', '11111']])
    const { result, rerender } = renderHook(({ sn }) => useVerification(sn), {
      initialProps: { sn: sn1 },
    })
    act(() => result.current.markVerified('p1'))
    expect(result.current.status('p1')).toBe('verified')

    rerender({ sn: new Map<string, string>() })
    expect(result.current.status('p1')).toBe('unverified')
  })

  it('after a "changed" flip, re-verifying re-snapshots and clears the warning', () => {
    const sn1 = new Map<string, string>([['p1', '11111']])
    const { result, rerender } = renderHook(({ sn }) => useVerification(sn), {
      initialProps: { sn: sn1 },
    })
    act(() => result.current.markVerified('p1'))

    const sn2 = new Map<string, string>([['p1', '22222']])
    rerender({ sn: sn2 })
    expect(result.current.status('p1')).toBe('changed')

    act(() => result.current.markVerified('p1'))
    expect(result.current.status('p1')).toBe('verified')

    // A subsequent identical-number rerender must not re-flip to 'changed'
    rerender({ sn: new Map(sn2) })
    expect(result.current.status('p1')).toBe('verified')
  })

  it('handles multiple peers independently', () => {
    const sn1 = new Map([['p1', '11'], ['p2', '22']])
    const { result, rerender } = renderHook(({ sn }) => useVerification(sn), {
      initialProps: { sn: sn1 },
    })
    act(() => result.current.markVerified('p1'))
    act(() => result.current.markVerified('p2'))

    const sn2 = new Map([['p1', '11'], ['p2', '99']])
    rerender({ sn: sn2 })

    expect(result.current.status('p1')).toBe('verified')
    expect(result.current.status('p2')).toBe('changed')
  })
})

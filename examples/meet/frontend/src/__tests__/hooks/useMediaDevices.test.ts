import { describe, it, expect, vi, beforeEach } from 'vitest'
import { renderHook, act } from '@testing-library/react'
import { useMediaDevices } from '@/hooks/useMediaDevices'

// ── Helpers ─────────────────────────────────────────────────────────────

async function flush(): Promise<void> {
  await act(async () => {
    await new Promise(r => setTimeout(r, 10))
  })
}

describe('useMediaDevices', () => {
  beforeEach(() => {
    // Reset getUserMedia spy between tests
    vi.restoreAllMocks()
  })

  it('acquires stream on mount and sets mediaReady', async () => {
    const { result } = renderHook(() => useMediaDevices())
    await flush()

    expect(result.current.mediaReady).toBe(true)
    expect(result.current.localStream).not.toBeNull()
  })

  it('initializes with video enabled when initialVideoEnabled=true', async () => {
    const { result } = renderHook(() => useMediaDevices({ initialVideoEnabled: true }))
    await flush()

    expect(result.current.videoEnabled).toBe(true)
    const videoTracks = result.current.localStream?.getVideoTracks() ?? []
    expect(videoTracks.length).toBeGreaterThan(0)
    expect(videoTracks[0].enabled).toBe(true)
  })

  it('initializes with audio enabled when initialAudioEnabled=true', async () => {
    const { result } = renderHook(() => useMediaDevices({ initialAudioEnabled: true }))
    await flush()

    expect(result.current.audioEnabled).toBe(true)
    const audioTracks = result.current.localStream?.getAudioTracks() ?? []
    expect(audioTracks.length).toBeGreaterThan(0)
    expect(audioTracks[0].enabled).toBe(true)
  })

  it('toggleVideo enables video track when off', async () => {
    const { result } = renderHook(() => useMediaDevices())
    await flush()

    expect(result.current.videoEnabled).toBe(false)

    await act(async () => {
      await result.current.toggleVideo()
    })

    expect(result.current.videoEnabled).toBe(true)
  })

  it('toggleVideo disables video track when on', async () => {
    const { result } = renderHook(() => useMediaDevices({ initialVideoEnabled: true }))
    await flush()

    expect(result.current.videoEnabled).toBe(true)

    await act(async () => {
      await result.current.toggleVideo()
    })

    expect(result.current.videoEnabled).toBe(false)
  })

  it('toggleAudio enables audio track when off', async () => {
    const { result } = renderHook(() => useMediaDevices())
    await flush()

    expect(result.current.audioEnabled).toBe(false)

    await act(async () => {
      await result.current.toggleAudio()
    })

    expect(result.current.audioEnabled).toBe(true)
  })

  it('toggleAudio disables audio track when on', async () => {
    const { result } = renderHook(() => useMediaDevices({ initialAudioEnabled: true }))
    await flush()

    await act(async () => {
      await result.current.toggleAudio()
    })

    expect(result.current.audioEnabled).toBe(false)
  })

  it('sets error when getUserMedia fails', async () => {
    const originalGUM = navigator.mediaDevices.getUserMedia
    navigator.mediaDevices.getUserMedia = vi.fn().mockRejectedValue(new Error('Permission denied'))

    const { result } = renderHook(() => useMediaDevices())
    await flush()

    expect(result.current.error).toBe('Permission denied')
    expect(result.current.mediaReady).toBe(true)
    expect(result.current.localStream).toBeNull()

    navigator.mediaDevices.getUserMedia = originalGUM
  })

  it('concurrent acquire returns same stream', async () => {
    const gumSpy = vi.spyOn(navigator.mediaDevices, 'getUserMedia')

    const { result } = renderHook(() => useMediaDevices())
    await flush()

    // getUserMedia should only have been called once despite mount effect
    expect(gumSpy.mock.calls.length).toBe(1)
  })

  it('unmount stops all tracks', async () => {
    const { result, unmount } = renderHook(() => useMediaDevices())
    await flush()

    const tracks = result.current.localStream?.getTracks() ?? []
    expect(tracks.length).toBeGreaterThan(0)

    unmount()

    for (const track of tracks) {
      expect(track.readyState).toBe('ended')
    }
  })

  it('updateVideoSettings calls applyConstraints', async () => {
    const { result } = renderHook(() => useMediaDevices({ initialVideoEnabled: true }))
    await flush()

    const videoTrack = result.current.localStream?.getVideoTracks()[0]
    const applySpy = vi.spyOn(videoTrack!, 'applyConstraints')

    act(() => {
      result.current.updateVideoSettings({ width: 1280, height: 720, frameRate: 30 })
    })

    expect(applySpy).toHaveBeenCalledWith({
      width: { ideal: 1280 },
      height: { ideal: 720 },
      frameRate: { ideal: 30 },
    })
  })

  it('startScreenShare returns a stream', async () => {
    const { result } = renderHook(() => useMediaDevices())
    await flush()

    let stream: MediaStream | null = null
    await act(async () => {
      stream = await result.current.startScreenShare()
    })

    expect(stream).not.toBeNull()
    expect(result.current.screenStream).not.toBeNull()
  })

  it('stopScreenShare stops tracks', async () => {
    const { result } = renderHook(() => useMediaDevices())
    await flush()

    await act(async () => {
      await result.current.startScreenShare()
    })

    const screenTracks = result.current.screenStream?.getTracks() ?? []

    act(() => {
      result.current.stopScreenShare()
    })

    expect(result.current.screenStream).toBeNull()
    for (const track of screenTracks) {
      expect(track.readyState).toBe('ended')
    }
  })
})

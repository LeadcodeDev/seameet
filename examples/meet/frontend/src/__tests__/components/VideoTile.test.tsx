import { describe, it, expect, afterEach } from 'vitest'
import { render, screen, cleanup } from '@testing-library/react'
import { VideoTile } from '@/components/VideoTile'
import React from 'react'

afterEach(cleanup)

function renderTile(props: Partial<React.ComponentProps<typeof VideoTile>> = {}) {
  const defaults: React.ComponentProps<typeof VideoTile> = {
    stream: null,
    name: 'Alice',
    isLocal: false,
    audioEnabled: true,
    videoEnabled: true,
    ...props,
  }
  return render(<VideoTile {...defaults} />)
}

describe('VideoTile', () => {
  it('shows video element when videoEnabled=true', () => {
    renderTile({ videoEnabled: true })
    const video = screen.getByTestId('video-element')
    expect(video.classList.contains('hidden')).toBe(false)
  })

  it('shows avatar when videoEnabled=false and not screen share', () => {
    renderTile({ videoEnabled: false, isScreenShare: false })
    expect(screen.getByTestId('avatar-placeholder')).toBeDefined()
    const video = screen.getByTestId('video-element')
    expect(video.classList.contains('hidden')).toBe(true)
  })

  it('screen share is always visible even if videoEnabled=false', () => {
    renderTile({ videoEnabled: false, isScreenShare: true })
    const video = screen.getByTestId('video-element')
    expect(video.classList.contains('hidden')).toBe(false)
    // No avatar for screen share
    expect(screen.queryByTestId('avatar-placeholder')).toBeNull()
  })

  it('shows mic-off indicator when !audioEnabled and not screen share', () => {
    const { container } = renderTile({ audioEnabled: false, isScreenShare: false })
    // MicOff icon is rendered inside a div
    const micOff = container.querySelector('.text-red-400')
    expect(micOff).not.toBeNull()
  })

  it('does not show mic-off for screen share', () => {
    const { container } = renderTile({ audioEnabled: false, isScreenShare: true })
    const micOff = container.querySelector('.text-red-400')
    expect(micOff).toBeNull()
  })

  it('shows E2EE badge when e2eeActive=true', () => {
    const { container } = renderTile({ e2eeActive: true })
    const shield = container.querySelector('.text-green-400')
    expect(shield).not.toBeNull()
  })

  it('does not show E2EE badge when e2eeActive=false', () => {
    const { container } = renderTile({ e2eeActive: false })
    const shield = container.querySelector('.text-green-400')
    expect(shield).toBeNull()
  })

  it('shows active speaker ring', () => {
    const { container } = renderTile({ isActiveSpeaker: true })
    const tile = container.querySelector('[data-testid="video-tile"]')!
    expect(tile.className).toContain('ring-green-500')
  })

  it('label shows "(You)" for local participant', () => {
    renderTile({ isLocal: true, name: 'Alice' })
    expect(screen.getByText('Alice (You)')).toBeDefined()
  })

  it('label shows name without "(You)" for remote', () => {
    renderTile({ isLocal: false, name: 'Bob' })
    expect(screen.getByText('Bob')).toBeDefined()
    expect(screen.queryByText('Bob (You)')).toBeNull()
  })

  it('label shows screen share format', () => {
    renderTile({ isScreenShare: true, name: 'Alice' })
    expect(screen.getByText("Alice's screen")).toBeDefined()
  })
})

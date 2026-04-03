import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, screen, fireEvent } from '@testing-library/react'
import { ControlBar } from '@/components/ControlBar'
import React from 'react'

// ── Mock useCall ────────────────────────────────────────────────────────

const mockLeave = vi.fn()
const mockToggleAudio = vi.fn()
const mockToggleVideo = vi.fn()
const mockStartScreenShare = vi.fn()
const mockStopScreenShare = vi.fn()

let callValues: Record<string, unknown>

vi.mock('@/context/CallContext', () => ({
  useCall: () => callValues,
}))

// Mock sub-components that use useCall
vi.mock('@/components/VideoSettingsPopover', () => ({
  VideoSettingsPopover: () => <div data-testid="video-settings" />,
}))
vi.mock('@/components/SafetyNumberPanel', () => ({
  SafetyNumberPanel: () => <div data-testid="safety-panel" />,
}))

// Mock Radix Tooltip to avoid TooltipProvider requirement
vi.mock('@/components/ui/tooltip', () => ({
  Tooltip: ({ children }: { children: React.ReactNode }) => <>{children}</>,
  TooltipTrigger: React.forwardRef(({ children, asChild, ...props }: { children: React.ReactNode; asChild?: boolean; [key: string]: unknown }, _ref: React.Ref<HTMLElement>) => {
    if (asChild && React.isValidElement(children)) return children
    return <span {...props}>{children}</span>
  }),
  TooltipContent: ({ children }: { children: React.ReactNode }) => <span>{children}</span>,
}))

function setCallValues(overrides: Record<string, unknown> = {}) {
  callValues = {
    audioEnabled: true,
    videoEnabled: true,
    toggleAudio: mockToggleAudio,
    toggleVideo: mockToggleVideo,
    startScreenShare: mockStartScreenShare,
    stopScreenShare: mockStopScreenShare,
    localScreenStream: null,
    leave: mockLeave,
    roomId: 'test-room',
    e2eeEnabled: false,
    ...overrides,
  }
}

describe('ControlBar', () => {
  beforeEach(() => {
    vi.clearAllMocks()
    setCallValues()
  })

  it('mic button calls toggleAudio', () => {
    render(<ControlBar />)
    fireEvent.click(screen.getByTestId('btn-toggle-mic'))
    expect(mockToggleAudio).toHaveBeenCalledOnce()
  })

  it('camera button calls toggleVideo', () => {
    render(<ControlBar />)
    fireEvent.click(screen.getByTestId('btn-toggle-camera'))
    expect(mockToggleVideo).toHaveBeenCalledOnce()
  })

  it('leave button calls leave', () => {
    render(<ControlBar />)
    fireEvent.click(screen.getByTestId('btn-leave'))
    expect(mockLeave).toHaveBeenCalledOnce()
  })

  it('screen share button calls startScreenShare when not sharing', () => {
    setCallValues({ localScreenStream: null })
    render(<ControlBar />)
    fireEvent.click(screen.getByTestId('btn-screen-share'))
    expect(mockStartScreenShare).toHaveBeenCalledOnce()
  })

  it('screen share button calls stopScreenShare when sharing', () => {
    setCallValues({ localScreenStream: new MediaStream() })
    render(<ControlBar />)
    fireEvent.click(screen.getByTestId('btn-screen-share'))
    expect(mockStopScreenShare).toHaveBeenCalledOnce()
  })

  it('has correct data-testid attributes', () => {
    render(<ControlBar />)
    expect(screen.getByTestId('btn-toggle-mic')).toBeDefined()
    expect(screen.getByTestId('btn-toggle-camera')).toBeDefined()
    expect(screen.getByTestId('btn-leave')).toBeDefined()
    expect(screen.getByTestId('btn-screen-share')).toBeDefined()
  })
})

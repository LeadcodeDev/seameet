import { describe, it, expect, vi, beforeEach } from 'vitest'
import { render, fireEvent, cleanup } from '@testing-library/react'
import { SafetyNumberPanel } from '@/components/SafetyNumberPanel'

interface MockCallValues {
  e2eeEnabled: boolean
  e2eePeerStates: Map<string, { ready: boolean }>
  e2eeSafetyNumbers: Map<string, string>
  remotePeers: Map<string, { displayName: string }>
  verificationStatus: (peerId: string) => 'unverified' | 'verified' | 'changed'
  markPeerVerified: (peerId: string) => void
  clearPeerVerification: (peerId: string) => void
}

let mockCallValues: MockCallValues

vi.mock('@/context/CallContext', () => ({
  useCall: () => mockCallValues,
}))

beforeEach(() => {
  cleanup()
})

function setup(overrides: Partial<MockCallValues> = {}) {
  mockCallValues = {
    e2eeEnabled: true,
    e2eePeerStates: new Map([['p1', { ready: true }]]),
    e2eeSafetyNumbers: new Map([['p1', '11111 22222 33333']]),
    remotePeers: new Map([['p1', { displayName: 'Bob' }]]),
    verificationStatus: () => 'unverified',
    markPeerVerified: vi.fn(),
    clearPeerVerification: vi.fn(),
    ...overrides,
  }
}

describe('SafetyNumberPanel', () => {
  it('renders nothing when E2EE is off', () => {
    setup({ e2eeEnabled: false })
    const { container } = render(<SafetyNumberPanel />)
    expect(container.querySelector('[data-testid="e2ee-indicator"]')).toBeNull()
  })

  it('shows the verify button when peer is unverified and ready', () => {
    setup()
    const { getByTestId } = render(<SafetyNumberPanel />)
    fireEvent.click(getByTestId('e2ee-indicator'))
    expect(getByTestId('btn-verify')).toBeTruthy()
  })

  it('clicking "Mark as verified" calls markPeerVerified with the peer id', () => {
    const markPeerVerified = vi.fn()
    setup({ markPeerVerified })
    const { getByTestId } = render(<SafetyNumberPanel />)
    fireEvent.click(getByTestId('e2ee-indicator'))
    fireEvent.click(getByTestId('btn-verify'))
    expect(markPeerVerified).toHaveBeenCalledWith('p1')
  })

  it('renders the verified badge and the unverify button when status is "verified"', () => {
    setup({ verificationStatus: () => 'verified' })
    const { getByTestId, queryByTestId } = render(<SafetyNumberPanel />)
    fireEvent.click(getByTestId('e2ee-indicator'))
    expect(getByTestId('verified-badge')).toBeTruthy()
    expect(getByTestId('btn-unverify')).toBeTruthy()
    expect(queryByTestId('btn-verify')).toBeNull()
  })

  it('renders the changed warning and offers Re-verify when status is "changed"', () => {
    setup({ verificationStatus: () => 'changed' })
    const { getByTestId } = render(<SafetyNumberPanel />)
    fireEvent.click(getByTestId('e2ee-indicator'))
    expect(getByTestId('changed-badge')).toBeTruthy()
    const verifyBtn = getByTestId('btn-verify')
    expect(verifyBtn.textContent).toContain('Re-verify')
    expect(getByTestId('btn-unverify')).toBeTruthy()
  })

  it('does not offer Mark-as-verified while the peer is still in handshake', () => {
    setup({
      e2eePeerStates: new Map([['p1', { ready: false }]]),
      verificationStatus: () => 'unverified',
    })
    const { getByTestId, queryByTestId } = render(<SafetyNumberPanel />)
    fireEvent.click(getByTestId('e2ee-indicator'))
    expect(queryByTestId('btn-verify')).toBeNull()
  })

  it('clicking "Clear verification" calls clearPeerVerification', () => {
    const clearPeerVerification = vi.fn()
    setup({ verificationStatus: () => 'verified', clearPeerVerification })
    const { getByTestId } = render(<SafetyNumberPanel />)
    fireEvent.click(getByTestId('e2ee-indicator'))
    fireEvent.click(getByTestId('btn-unverify'))
    expect(clearPeerVerification).toHaveBeenCalledWith('p1')
  })
})

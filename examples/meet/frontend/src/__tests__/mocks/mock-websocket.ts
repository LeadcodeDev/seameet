import type { SignalingMessage } from '@/types'

export class MockWebSocket {
  static readonly CONNECTING = 0
  static readonly OPEN = 1
  static readonly CLOSING = 2
  static readonly CLOSED = 3

  readonly CONNECTING = 0
  readonly OPEN = 1
  readonly CLOSING = 2
  readonly CLOSED = 3

  readyState: number = MockWebSocket.CONNECTING
  url: string
  protocol = ''
  bufferedAmount = 0
  extensions = ''
  binaryType: BinaryType = 'blob'

  onopen: ((ev: Event) => void) | null = null
  onclose: ((ev: CloseEvent) => void) | null = null
  onmessage: ((ev: MessageEvent) => void) | null = null
  onerror: ((ev: Event) => void) | null = null

  private _sent: SignalingMessage[] = []

  constructor(url: string | URL, _protocols?: string | string[]) {
    this.url = typeof url === 'string' ? url : url.toString()
    // Auto-fire onopen on microtask so hooks see 'open' state
    queueMicrotask(() => {
      this.readyState = MockWebSocket.OPEN
      this.onopen?.(new Event('open'))
    })
  }

  send(data: string | ArrayBufferLike | Blob | ArrayBufferView): void {
    if (this.readyState !== MockWebSocket.OPEN) return
    this._sent.push(JSON.parse(data as string))
  }

  close(_code?: number, _reason?: string): void {
    this.readyState = MockWebSocket.CLOSED
    this.onclose?.(new CloseEvent('close'))
  }

  /** Drain all sent messages and return them */
  drain(): SignalingMessage[] {
    const msgs = [...this._sent]
    this._sent = []
    return msgs
  }

  /** Simulate a message from the server */
  serverPush(msg: SignalingMessage): void {
    this.onmessage?.(new MessageEvent('message', { data: JSON.stringify(msg) }))
  }

  // Stubs for EventTarget interface
  addEventListener(): void {}
  removeEventListener(): void {}
  dispatchEvent(): boolean { return true }
}

/** Get the most recent MockWebSocket instance created by the global mock */
let _lastInstance: MockWebSocket | null = null

export function getLastMockWebSocket(): MockWebSocket {
  if (!_lastInstance) throw new Error('No MockWebSocket instance created')
  return _lastInstance
}

export function installMockWebSocket(): void {
  const OriginalWebSocket = globalThis.WebSocket
  // @ts-expect-error — replacing global WebSocket with mock
  globalThis.WebSocket = class extends MockWebSocket {
    constructor(url: string | URL, protocols?: string | string[]) {
      super(url, protocols)
      _lastInstance = this
    }
  }
  // Copy static constants
  Object.defineProperty(globalThis.WebSocket, 'CONNECTING', { value: 0 })
  Object.defineProperty(globalThis.WebSocket, 'OPEN', { value: 1 })
  Object.defineProperty(globalThis.WebSocket, 'CLOSING', { value: 2 })
  Object.defineProperty(globalThis.WebSocket, 'CLOSED', { value: 3 })

  return OriginalWebSocket as unknown as void
}

export function resetMockWebSocket(): void {
  _lastInstance = null
}

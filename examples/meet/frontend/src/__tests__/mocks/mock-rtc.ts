let _midCounter = 0

export function resetMidCounter(): void {
  _midCounter = 0
}

class MockRTCRtpSender {
  track: MediaStreamTrack | null
  constructor(track: MediaStreamTrack | null) {
    this.track = track
  }
  replaceTrack(_track: MediaStreamTrack | null): Promise<void> { return Promise.resolve() }
  getParameters(): RTCRtpSendParameters {
    return {
      encodings: [{ maxBitrate: 800000 }],
      transactionId: '',
      codecs: [],
      headerExtensions: [],
      rtcp: { cname: '', reducedSize: false },
    } as unknown as RTCRtpSendParameters
  }
  setParameters(_params: RTCRtpSendParameters): Promise<void> { return Promise.resolve() }
}

class MockRTCRtpReceiver {
  track: MediaStreamTrack
  constructor(kind: string) {
    this.track = new MockMediaStreamTrack(kind, `recv-${kind}-${_midCounter}`)
  }
}

export class MockRTCRtpTransceiver {
  mid: string | null
  sender: MockRTCRtpSender
  receiver: MockRTCRtpReceiver
  direction: RTCRtpTransceiverDirection = 'sendrecv'
  currentDirection: RTCRtpTransceiverDirection | null = 'sendrecv'

  constructor(kind: string, init?: RTCRtpTransceiverInit) {
    this.mid = String(_midCounter++)
    this.sender = new MockRTCRtpSender(null)
    this.receiver = new MockRTCRtpReceiver(kind)
    if (init?.direction) this.direction = init.direction
  }

  stop(): void { this.direction = 'inactive' }
}

class MockMediaStreamTrack {
  kind: string
  id: string
  enabled = true
  readyState: MediaStreamTrackState = 'live'
  muted = false
  label: string

  constructor(kind: string, id?: string) {
    this.kind = kind
    this.id = id ?? `track-${kind}-${Math.random().toString(36).slice(2, 8)}`
    this.label = this.id
  }

  stop(): void { this.readyState = 'ended' }
  applyConstraints(_constraints?: MediaTrackConstraints): Promise<void> { return Promise.resolve() }
  clone(): MockMediaStreamTrack { return new MockMediaStreamTrack(this.kind, `${this.id}-clone`) }
  getConstraints(): MediaTrackConstraints { return {} }
  getCapabilities(): MediaTrackCapabilities { return {} as MediaTrackCapabilities }
  getSettings(): MediaTrackSettings { return {} as MediaTrackSettings }
  addEventListener(): void {}
  removeEventListener(): void {}
  dispatchEvent(): boolean { return true }
}

export class MockRTCPeerConnection {
  private _transceivers: MockRTCRtpTransceiver[] = []
  private _senders: MockRTCRtpSender[] = []

  connectionState: RTCPeerConnectionState = 'new'
  iceConnectionState: RTCIceConnectionState = 'new'
  signalingState: RTCSignalingState = 'stable'
  localDescription: RTCSessionDescription | null = null
  remoteDescription: RTCSessionDescription | null = null

  onicecandidate: ((ev: RTCPeerConnectionIceEvent) => void) | null = null
  ontrack: ((ev: RTCTrackEvent) => void) | null = null
  onconnectionstatechange: (() => void) | null = null
  oniceconnectionstatechange: (() => void) | null = null
  onsignalingstatechange: (() => void) | null = null
  ondatachannel: ((ev: RTCDataChannelEvent) => void) | null = null
  onicegatheringstatechange: (() => void) | null = null
  onnegotiationneeded: (() => void) | null = null
  onicecandidateerror: ((ev: Event) => void) | null = null

  constructor(_config?: RTCConfiguration) {}

  addTransceiver(trackOrKind: MediaStreamTrack | string, init?: RTCRtpTransceiverInit): MockRTCRtpTransceiver {
    const kind = typeof trackOrKind === 'string' ? trackOrKind : trackOrKind.kind
    const t = new MockRTCRtpTransceiver(kind, init)
    if (typeof trackOrKind !== 'string') {
      t.sender = new MockRTCRtpSender(trackOrKind)
    }
    this._transceivers.push(t)
    return t
  }

  addTrack(track: MediaStreamTrack, ..._streams: MediaStream[]): RTCRtpSender {
    const sender = new MockRTCRtpSender(track)
    this._senders.push(sender)
    // Also create a transceiver for addTrack
    const t = new MockRTCRtpTransceiver(track.kind)
    t.sender = sender
    this._transceivers.push(t)
    return sender as unknown as RTCRtpSender
  }

  removeTrack(_sender: RTCRtpSender): void {}

  getTransceivers(): MockRTCRtpTransceiver[] { return [...this._transceivers] }
  getSenders(): MockRTCRtpSender[] {
    return [...this._senders, ...this._transceivers.map(t => t.sender)]
  }
  getReceivers(): MockRTCRtpReceiver[] { return this._transceivers.map(t => t.receiver) }

  async createOffer(_options?: RTCOfferOptions): Promise<RTCSessionDescriptionInit> {
    return { type: 'offer', sdp: 'mock-sdp-offer' }
  }

  async createAnswer(_options?: RTCAnswerOptions): Promise<RTCSessionDescriptionInit> {
    return { type: 'answer', sdp: 'mock-sdp-answer' }
  }

  async setLocalDescription(desc: RTCSessionDescriptionInit): Promise<void> {
    this.localDescription = desc as RTCSessionDescription
  }

  async setRemoteDescription(desc: RTCSessionDescriptionInit): Promise<void> {
    this.remoteDescription = desc as RTCSessionDescription
  }

  async addIceCandidate(_candidate?: RTCIceCandidateInit): Promise<void> {}

  close(): void {
    this.connectionState = 'closed'
  }

  getConfiguration(): RTCConfiguration { return {} }
  getStats(): Promise<RTCStatsReport> { return Promise.resolve(new Map() as unknown as RTCStatsReport) }
  setConfiguration(_config: RTCConfiguration): void {}
  createDataChannel(_label: string, _options?: RTCDataChannelInit): RTCDataChannel { return {} as RTCDataChannel }
  restartIce(): void {}
  addEventListener(): void {}
  removeEventListener(): void {}
  dispatchEvent(): boolean { return true }
}

export function installMockRTC(): void {
  // @ts-expect-error — replacing global
  globalThis.RTCPeerConnection = MockRTCPeerConnection
  // @ts-expect-error — replacing global
  globalThis.RTCSessionDescription = class {
    type: string; sdp: string
    constructor(init: RTCSessionDescriptionInit) { this.type = init.type!; this.sdp = init.sdp! }
    toJSON() { return { type: this.type, sdp: this.sdp } }
  }
  // @ts-expect-error — replacing global
  globalThis.RTCIceCandidate = class {
    candidate: string; sdpMid: string | null; sdpMLineIndex: number | null
    constructor(init: RTCIceCandidateInit) {
      this.candidate = init.candidate!
      this.sdpMid = init.sdpMid ?? null
      this.sdpMLineIndex = init.sdpMLineIndex ?? null
    }
    toJSON() { return { candidate: this.candidate, sdpMid: this.sdpMid, sdpMLineIndex: this.sdpMLineIndex } }
  }
}

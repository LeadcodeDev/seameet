export class MockMediaStreamTrack {
  kind: string
  id: string
  enabled = true
  readyState: MediaStreamTrackState = 'live'
  muted = false
  label: string

  private _listeners = new Map<string, Set<EventListener>>()

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

  addEventListener(type: string, listener: EventListener): void {
    if (!this._listeners.has(type)) this._listeners.set(type, new Set())
    this._listeners.get(type)!.add(listener)
  }
  removeEventListener(type: string, listener: EventListener): void {
    this._listeners.get(type)?.delete(listener)
  }
  dispatchEvent(event: Event): boolean {
    this._listeners.get(event.type)?.forEach(l => l(event))
    return true
  }
}

export class MockMediaStream {
  id: string
  private _tracks: MockMediaStreamTrack[] = []

  constructor(tracksOrId?: MockMediaStreamTrack[] | string) {
    if (typeof tracksOrId === 'string') {
      this.id = tracksOrId
    } else {
      this.id = `stream-${Math.random().toString(36).slice(2, 8)}`
      if (tracksOrId) this._tracks = [...tracksOrId]
    }
  }

  getTracks(): MockMediaStreamTrack[] { return [...this._tracks] }
  getAudioTracks(): MockMediaStreamTrack[] { return this._tracks.filter(t => t.kind === 'audio') }
  getVideoTracks(): MockMediaStreamTrack[] { return this._tracks.filter(t => t.kind === 'video') }
  addTrack(track: MockMediaStreamTrack): void {
    if (!this._tracks.find(t => t.id === track.id)) this._tracks.push(track)
  }
  removeTrack(track: MockMediaStreamTrack): void {
    this._tracks = this._tracks.filter(t => t.id !== track.id)
  }
  clone(): MockMediaStream {
    return new MockMediaStream(this._tracks.map(t => t.clone()))
  }
  getTrackById(id: string): MockMediaStreamTrack | null {
    return this._tracks.find(t => t.id === id) ?? null
  }

  addEventListener(): void {}
  removeEventListener(): void {}
  dispatchEvent(): boolean { return true }
  get active(): boolean { return this._tracks.some(t => t.readyState === 'live') }
}

export function createMockStream(kinds: ('audio' | 'video')[] = ['audio', 'video']): MockMediaStream {
  return new MockMediaStream(kinds.map(k => new MockMediaStreamTrack(k)))
}

export function installMockMedia(): void {
  // @ts-expect-error — replacing global
  globalThis.MediaStream = MockMediaStream

  if (!navigator.mediaDevices) {
    // @ts-expect-error — creating mediaDevices
    navigator.mediaDevices = {}
  }

  navigator.mediaDevices.getUserMedia = async (_constraints?: MediaStreamConstraints) => {
    return createMockStream(['audio', 'video']) as unknown as MediaStream
  }

  navigator.mediaDevices.getDisplayMedia = async (_constraints?: DisplayMediaStreamOptions) => {
    return createMockStream(['video']) as unknown as MediaStream
  }
}

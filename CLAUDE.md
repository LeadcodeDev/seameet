# seameet

WebRTC video conferencing app built on a custom Rust SFU.

## Architecture

- **SFU server**: Rust crates in `crates/`
- **Frontend**: React + TypeScript in `examples/meet/frontend/`
- **Signaling**: WebSocket-based, messages defined in `src/types/index.ts`

## Development

```bash
# Frontend
cd examples/meet/frontend
npm install
npm run dev

# Run tests
npx vitest run           # unit tests
npx playwright test      # E2E tests
```

## Video/Camera Acceptance Criteria

These rules define correct behavior for video/camera across all participants. Any change to video, WebRTC, or signaling code MUST preserve these invariants.

### Join scenarios

- **Camera ON at join**: participant's tile shows live video to all others immediately.
- **Camera OFF at join**: participant's tile shows avatar placeholder (initials) to all others. The `<video>` element gets the `.hidden` class; an `<Avatar>` with `bg-primary` is rendered instead.
- Initial mute state is sent via `mute_video`/`unmute_video` signaling messages on join.

### Toggle camera OFF (in-room)

- Local video track is stopped; `mute_video` signal is sent.
- All remote participants see the avatar placeholder replace the video on that tile.
- The participant's own tile also shows the placeholder.

### Toggle camera ON (in-room)

- A new video track is acquired; `unmute_video` signal is sent.
- `webrtc.replaceLocalTracks(media.localStream)` MUST be called so the SFU receives the new track without full renegotiation.
- All remote participants see live video replace the placeholder on that tile.
- The participant's own tile shows their live video.

### Multi-participant invariant (3+ participants)

- Each participant independently controls their own camera.
- Toggling one participant's camera MUST NOT affect any other participant's video state.
- A newly-joining participant sees the correct state (video or placeholder) for every existing participant.

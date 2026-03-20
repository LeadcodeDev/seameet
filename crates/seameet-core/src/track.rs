use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::broadcast;

/// Unique identifier for a media track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrackId(pub u32);

/// Direction of a media track.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackDirection {
    /// Track can only send media.
    SendOnly,
    /// Track can only receive media.
    RecvOnly,
    /// Track can both send and receive media.
    SendRecv,
}

/// Discriminant for the kind of video track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VideoTrackKind {
    /// Webcam / front-facing camera.
    Camera,
    /// Screen or window share.
    Screen,
}

/// Shared inner state for a track handle.
struct TrackHandleInner {
    muted: AtomicBool,
    stop_tx: broadcast::Sender<()>,
}

/// Handle to a media track with mute/unmute and close capabilities.
///
/// Cloning a [`TrackHandle`] shares the same underlying state (mute flag and
/// stop signal), so all clones observe the same mute/close status.
#[derive(Debug, Clone)]
pub struct TrackHandle {
    /// The track identifier.
    id: TrackId,
    /// The kind of video track (camera or screen).
    kind: VideoTrackKind,
    /// Shared inner state.
    inner: Arc<TrackHandleInner>,
}

impl std::fmt::Debug for TrackHandleInner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TrackHandleInner")
            .field("muted", &self.muted.load(Ordering::SeqCst))
            .finish()
    }
}

impl TrackHandle {
    /// Creates a new [`TrackHandle`] for the given [`TrackId`] with
    /// [`VideoTrackKind::Camera`] as the default kind.
    pub fn new(id: TrackId) -> Self {
        Self::with_kind(id, VideoTrackKind::Camera)
    }

    /// Creates a new [`TrackHandle`] with a specific [`VideoTrackKind`].
    pub fn with_kind(id: TrackId, kind: VideoTrackKind) -> Self {
        let (stop_tx, _) = broadcast::channel(1);
        Self {
            id,
            kind,
            inner: Arc::new(TrackHandleInner {
                muted: AtomicBool::new(false),
                stop_tx,
            }),
        }
    }

    /// Returns the track identifier.
    pub fn id(&self) -> TrackId {
        self.id
    }

    /// Returns the video track kind (camera or screen).
    pub fn kind(&self) -> VideoTrackKind {
        self.kind
    }

    /// Mutes this track.
    pub fn mute(&self) {
        self.inner.muted.store(true, Ordering::SeqCst);
    }

    /// Unmutes this track.
    pub fn unmute(&self) {
        self.inner.muted.store(false, Ordering::SeqCst);
    }

    /// Returns `true` if the track is currently muted.
    pub fn is_muted(&self) -> bool {
        self.inner.muted.load(Ordering::SeqCst)
    }

    /// Closes the track by broadcasting a stop signal to all subscribers.
    pub fn close(&self) {
        let _ = self.inner.stop_tx.send(());
    }

    /// Returns a receiver that will be notified when the track is closed.
    pub fn subscribe_stop(&self) -> broadcast::Receiver<()> {
        self.inner.stop_tx.subscribe()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mute_unmute() {
        let handle = TrackHandle::new(TrackId(1));
        assert!(!handle.is_muted());
        handle.mute();
        assert!(handle.is_muted());
        handle.unmute();
        assert!(!handle.is_muted());
    }

    #[test]
    fn clone_shares_state() {
        let handle = TrackHandle::new(TrackId(2));
        let clone = handle.clone();
        handle.mute();
        assert!(clone.is_muted());
    }

    #[tokio::test]
    async fn close_notifies_subscriber() {
        let handle = TrackHandle::new(TrackId(3));
        let mut rx = handle.subscribe_stop();
        handle.close();
        assert!(rx.recv().await.is_ok());
    }

    #[test]
    fn track_id_equality() {
        assert_eq!(TrackId(42), TrackId(42));
        assert_ne!(TrackId(1), TrackId(2));
    }

    #[test]
    fn track_direction_debug() {
        assert_eq!(format!("{:?}", TrackDirection::SendOnly), "SendOnly");
        assert_eq!(format!("{:?}", TrackDirection::RecvOnly), "RecvOnly");
        assert_eq!(format!("{:?}", TrackDirection::SendRecv), "SendRecv");
    }

    #[test]
    fn test_track_handle_screen_kind() {
        let handle = TrackHandle::with_kind(TrackId(10), VideoTrackKind::Screen);
        assert_eq!(handle.kind(), VideoTrackKind::Screen);
        assert_eq!(handle.id(), TrackId(10));
    }

    #[test]
    fn test_default_kind_is_camera() {
        let handle = TrackHandle::new(TrackId(11));
        assert_eq!(handle.kind(), VideoTrackKind::Camera);
    }

    #[test]
    fn test_video_track_kind_serde() {
        let json = serde_json::to_string(&VideoTrackKind::Screen).expect("ser");
        assert_eq!(json, "\"screen\"");
        let back: VideoTrackKind = serde_json::from_str(&json).expect("de");
        assert_eq!(back, VideoTrackKind::Screen);

        let json2 = serde_json::to_string(&VideoTrackKind::Camera).expect("ser");
        assert_eq!(json2, "\"camera\"");
    }
}

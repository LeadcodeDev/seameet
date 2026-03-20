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

/// Handle to a media track with mute/unmute and close capabilities.
///
/// Cloning a [`TrackHandle`] shares the same underlying state (mute flag and
/// stop signal), so all clones observe the same mute/close status.
#[derive(Debug, Clone)]
pub struct TrackHandle {
    /// The track identifier.
    id: TrackId,
    /// Shared mute state.
    muted: Arc<AtomicBool>,
    /// Sender side of the stop signal. Dropping all senders (or calling
    /// [`TrackHandle::close`]) notifies every receiver that the track is done.
    stop_tx: Arc<broadcast::Sender<()>>,
}

impl TrackHandle {
    /// Creates a new [`TrackHandle`] for the given [`TrackId`].
    pub fn new(id: TrackId) -> Self {
        let (stop_tx, _) = broadcast::channel(1);
        Self {
            id,
            muted: Arc::new(AtomicBool::new(false)),
            stop_tx: Arc::new(stop_tx),
        }
    }

    /// Returns the track identifier.
    pub fn id(&self) -> TrackId {
        self.id
    }

    /// Mutes this track.
    pub fn mute(&self) {
        self.muted.store(true, Ordering::SeqCst);
    }

    /// Unmutes this track.
    pub fn unmute(&self) {
        self.muted.store(false, Ordering::SeqCst);
    }

    /// Returns `true` if the track is currently muted.
    pub fn is_muted(&self) -> bool {
        self.muted.load(Ordering::SeqCst)
    }

    /// Closes the track by broadcasting a stop signal to all subscribers.
    pub fn close(&self) {
        let _ = self.stop_tx.send(());
    }

    /// Returns a receiver that will be notified when the track is closed.
    pub fn subscribe_stop(&self) -> broadcast::Receiver<()> {
        self.stop_tx.subscribe()
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
}

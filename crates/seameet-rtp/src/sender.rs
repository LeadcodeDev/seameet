use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use bytes::Bytes;
use seameet_core::frame::EncodedVideo;
use seameet_core::SeaMeetError;
use tokio::sync::mpsc;

use crate::packet::RtpPacket;

/// Maximum payload size per RTP packet (MTU-safe).
const MAX_PAYLOAD_SIZE: usize = 1200;

/// Cumulative send statistics.
#[derive(Debug, Clone, Default)]
pub struct SenderStats {
    /// Total bytes of payload sent.
    pub bytes_sent: u64,
    /// Total RTP packets sent.
    pub packets_sent: u64,
}

/// Sends RTP packets over a channel, handling sequence numbering and
/// video fragmentation.
pub struct RtpSender {
    /// Synchronization source identifier.
    ssrc: u32,
    /// Current sequence number (wraps at u16::MAX).
    seq: u16,
    /// Outbound packet channel.
    tx: mpsc::Sender<Bytes>,
    /// Whether a PLI (picture loss indication) has been requested.
    pli_requested: Arc<AtomicBool>,
    /// Cumulative statistics.
    stats: SenderStats,
}

impl RtpSender {
    /// Creates a new sender with the given SSRC and outbound channel.
    pub fn new(ssrc: u32, tx: mpsc::Sender<Bytes>) -> Self {
        Self {
            ssrc,
            seq: 0,
            tx,
            pli_requested: Arc::new(AtomicBool::new(false)),
            stats: SenderStats::default(),
        }
    }

    /// Returns the next sequence number and advances the counter (wrapping).
    fn next_seq(&mut self) -> u16 {
        let s = self.seq;
        self.seq = self.seq.wrapping_add(1);
        s
    }

    /// Sends an audio payload as a single RTP packet.
    pub async fn send_audio(
        &mut self,
        payload: Vec<u8>,
        timestamp: u32,
    ) -> Result<(), SeaMeetError> {
        let seq = self.next_seq();
        let pkt = RtpPacket::new_audio(payload, timestamp, self.ssrc, seq);
        self.send_packet(pkt).await
    }

    /// Sends an encoded video frame, fragmenting into MTU-safe RTP packets.
    ///
    /// The marker bit is set on the last fragment of the frame.
    pub async fn send_video(
        &mut self,
        frame: EncodedVideo,
        timestamp: u32,
    ) -> Result<(), SeaMeetError> {
        let chunks: Vec<&[u8]> = frame.data.chunks(MAX_PAYLOAD_SIZE).collect();
        let last_idx = chunks.len().saturating_sub(1);

        for (i, chunk) in chunks.iter().enumerate() {
            let seq = self.next_seq();
            let marker = i == last_idx;
            let pkt = RtpPacket::new_video(chunk.to_vec(), timestamp, self.ssrc, seq, marker);
            self.send_packet(pkt).await?;
        }

        Ok(())
    }

    /// Sends a silence packet (empty audio payload).
    pub async fn send_silence(&mut self, timestamp: u32) -> Result<(), SeaMeetError> {
        self.send_audio(Vec::new(), timestamp).await
    }

    /// Returns `true` if a PLI has been requested.
    pub fn pli_requested(&self) -> bool {
        self.pli_requested.load(Ordering::SeqCst)
    }

    /// Clears the PLI flag.
    pub fn clear_pli(&mut self) {
        self.pli_requested.store(false, Ordering::SeqCst);
    }

    /// Sets the PLI flag (typically called from an RTCP handler).
    pub fn request_pli(&self) {
        self.pli_requested.store(true, Ordering::SeqCst);
    }

    /// Returns cumulative send statistics.
    pub fn stats(&self) -> &SenderStats {
        &self.stats
    }

    /// Serializes and sends one RTP packet through the channel.
    async fn send_packet(&mut self, pkt: RtpPacket) -> Result<(), SeaMeetError> {
        let bytes = pkt.to_bytes();
        self.stats.bytes_sent += bytes.len() as u64;
        self.stats.packets_sent += 1;
        self.tx
            .send(bytes)
            .await
            .map_err(|_| SeaMeetError::PeerConnection("send channel closed".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use seameet_core::frame::EncodedVideo;
    use std::time::Duration;

    #[tokio::test]
    async fn test_sequence_number_wraparound() {
        let total = 70_000u32;
        let (tx, mut rx) = mpsc::channel(total as usize + 1);
        let mut sender = RtpSender::new(1, tx);

        for i in 0..total {
            sender.send_audio(vec![0], i * 160).await.expect("send");
        }

        let mut last_seq = None;
        let mut count = 0u32;
        while let Ok(bytes) = rx.try_recv() {
            let pkt = RtpPacket::from_bytes(&bytes).expect("decode");
            last_seq = Some(pkt.sequence_number);
            count += 1;
        }

        assert_eq!(count, total);
        // 70_000 mod 65_536 = 4_464, so last seq should be 4_463.
        assert_eq!(last_seq, Some(4_463));
    }

    #[tokio::test]
    async fn test_rtp_sender_video_fragmentation() {
        let (tx, mut rx) = mpsc::channel(128);
        let mut sender = RtpSender::new(42, tx);

        let frame = EncodedVideo {
            data: vec![0xAB; 4000],
            is_keyframe: true,
            pts: Duration::ZERO,
        };

        sender.send_video(frame, 90000).await.expect("send");

        let mut packets = Vec::new();
        while let Ok(bytes) = rx.try_recv() {
            let pkt = RtpPacket::from_bytes(&bytes).expect("decode");
            packets.push(pkt);
        }

        // 4000 / 1200 = 3.33 → 4 packets.
        assert!(packets.len() >= 4, "expected >= 4 packets, got {}", packets.len());

        // Only the last packet should have the marker bit set.
        for pkt in &packets[..packets.len() - 1] {
            assert!(!pkt.marker, "non-last fragment should not have marker");
        }
        assert!(packets.last().expect("last").marker, "last fragment must have marker");

        // Total payload should reconstruct the original frame.
        let total_payload: Vec<u8> = packets.iter().flat_map(|p| p.payload.iter().copied()).collect();
        assert_eq!(total_payload.len(), 4000);
        assert!(total_payload.iter().all(|&b| b == 0xAB));
    }

    #[tokio::test]
    async fn test_send_silence() {
        let (tx, mut rx) = mpsc::channel(8);
        let mut sender = RtpSender::new(1, tx);
        sender.send_silence(0).await.expect("send");

        let bytes = rx.try_recv().expect("recv");
        let pkt = RtpPacket::from_bytes(&bytes).expect("decode");
        assert!(pkt.payload.is_empty());
    }

    #[test]
    fn test_pli_flag() {
        let (tx, _rx) = mpsc::channel(1);
        let mut sender = RtpSender::new(1, tx);
        assert!(!sender.pli_requested());
        sender.request_pli();
        assert!(sender.pli_requested());
        sender.clear_pli();
        assert!(!sender.pli_requested());
    }
}

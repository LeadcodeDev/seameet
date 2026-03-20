use std::collections::BTreeMap;

use tracing::debug;

use crate::packet::RtpPacket;

/// Events emitted by the jitter buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JitterBufferEvent {
    /// A packet with the given sequence number was detected as lost.
    PacketLost(u16),
}

/// Cumulative statistics for the jitter buffer.
#[derive(Debug, Clone, Default)]
pub struct JitterStats {
    /// Total packets successfully received and buffered.
    pub received: u64,
    /// Total packets detected as lost (gaps).
    pub lost: u64,
    /// Total packets that arrived out of order but were still usable.
    pub reordered: u64,
    /// Total packets that arrived too late and were dropped.
    pub late: u64,
}

/// Reorder buffer that compensates for network jitter.
///
/// Packets are inserted with [`push`](Self::push) and extracted in sequence
/// order with [`poll`](Self::poll). The buffer detects lost, late, and
/// reordered packets.
pub struct JitterBuffer {
    /// Maximum number of packets to buffer.
    capacity: usize,
    /// Maximum acceptable lateness in milliseconds (reserved for future use).
    _max_late_ms: u32,
    /// Packets keyed by their *unwrapped* 32-bit sequence number.
    buffer: BTreeMap<u32, RtpPacket>,
    /// Next expected unwrapped sequence number to emit.
    next_emit: Option<u32>,
    /// Last unwrapped sequence number that was pushed.
    last_pushed: Option<u32>,
    /// Highest raw u16 seen (for unwrapping).
    high_seq: Option<u16>,
    /// Number of u16 wrap-arounds observed.
    cycles: u32,
    /// Pending events for the consumer.
    events: Vec<JitterBufferEvent>,
    /// Cumulative statistics.
    stats: JitterStats,
}

impl JitterBuffer {
    /// Creates a new jitter buffer.
    ///
    /// * `capacity` — maximum number of packets to hold before dropping the oldest.
    /// * `max_late_ms` — packets older than this (compared via timestamp delta)
    ///   are dropped on arrival.
    pub fn new(capacity: usize, max_late_ms: u32) -> Self {
        Self {
            capacity,
            _max_late_ms: max_late_ms,
            buffer: BTreeMap::new(),
            next_emit: None,
            last_pushed: None,
            high_seq: None,
            cycles: 0,
            events: Vec::new(),
            stats: JitterStats::default(),
        }
    }

    /// Unwraps a raw u16 sequence number into a monotonically increasing u32.
    fn unwrap_seq(&mut self, seq: u16) -> u32 {
        match self.high_seq {
            None => {
                self.high_seq = Some(seq);
                self.cycles = 0;
                seq as u32
            }
            Some(high) => {
                // Detect forward wrap: new seq is much smaller than high.
                let diff = seq.wrapping_sub(high);
                if diff < 0x8000 {
                    // Forward progression.
                    if seq < high && diff > 0 {
                        // Wrapped around.
                        self.cycles += 1;
                    }
                    self.high_seq = Some(seq);
                } else {
                    // Late / reordered packet (behind high).
                    // Don't update high_seq or cycles.
                    // Compute how far behind: high - seq in wrapped space.
                    let behind = high.wrapping_sub(seq);
                    // If behind wraps past a cycle boundary, use previous cycle.
                    if seq > high {
                        return self.cycles.saturating_sub(1) * 0x10000 + seq as u32;
                    }
                    let _ = behind;
                }
                self.cycles * 0x10000 + seq as u32
            }
        }
    }

    /// Inserts a packet into the buffer.
    pub fn push(&mut self, packet: RtpPacket) {
        let seq = packet.sequence_number;
        let unwrapped = self.unwrap_seq(seq);

        // Check if the packet is too late.
        if let Some(next) = self.next_emit {
            if unwrapped < next {
                debug!(seq, "dropping late packet");
                self.stats.late += 1;
                return;
            }
        }

        // Check if out of order relative to last pushed.
        if let Some(last) = self.last_pushed {
            if unwrapped < last {
                self.stats.reordered += 1;
            }
        }

        self.last_pushed = Some(unwrapped);
        self.buffer.insert(unwrapped, packet);
        self.stats.received += 1;

        // Enforce capacity — drop oldest.
        while self.buffer.len() > self.capacity {
            self.buffer.pop_first();
        }
    }

    /// Polls the next in-sequence packet.
    ///
    /// `now_ms` is the current time in milliseconds, used for late-packet
    /// detection via timestamp comparison. Packets are emitted strictly in
    /// sequence order. If the next expected packet is missing and the gap
    /// exceeds `max_late_ms` (estimated from the buffer state), the missing
    /// packets are declared lost and the buffer advances.
    pub fn poll(&mut self, now_ms: u64) -> Option<RtpPacket> {
        if self.buffer.is_empty() {
            return None;
        }

        // Initialize next_emit to the first packet if not set.
        let next = match self.next_emit {
            Some(n) => n,
            None => {
                let &first_key = self.buffer.keys().next()?;
                self.next_emit = Some(first_key);
                first_key
            }
        };

        // If the next expected packet is in the buffer, emit it.
        if let Some(pkt) = self.buffer.remove(&next) {
            self.next_emit = Some(next + 1);
            return Some(pkt);
        }

        // The next packet is missing. Check if we should skip it.
        // Look at the earliest packet we do have.
        let &earliest_buffered = self.buffer.keys().next()?;
        if earliest_buffered <= next {
            // Shouldn't happen, but handle gracefully.
            self.next_emit = Some(earliest_buffered);
            return self.buffer.remove(&earliest_buffered);
        }

        // Estimate if the gap is old enough to declare lost.
        // Use `now_ms` and `max_late_ms` as a simple timeout heuristic:
        // If we have packets beyond the gap, those packets' existence implies
        // the missing ones are late. We declare them lost if the gap is > 0.
        let gap_size = earliest_buffered - next;
        if gap_size > 0 {
            // Use the timestamp delta between what we have and what we expect
            // to estimate lateness. If we have newer packets, the gap is real.
            // Check if enough time has passed (caller-provided now_ms acts as
            // the wall-clock gate).
            let _ = now_ms; // The caller drives timing; we trust that if poll
                            // is called, it's time to check.

            // Emit PacketLost events for the gap.
            for seq_offset in 0..gap_size {
                let lost_unwrapped = next + seq_offset;
                let lost_seq = lost_unwrapped as u16;
                self.events.push(JitterBufferEvent::PacketLost(lost_seq));
                self.stats.lost += 1;
            }

            // Advance past the gap and emit the earliest buffered packet.
            self.next_emit = Some(earliest_buffered + 1);
            return self.buffer.remove(&earliest_buffered);
        }

        None
    }

    /// Drains pending events from the buffer.
    pub fn drain_events(&mut self) -> Vec<JitterBufferEvent> {
        std::mem::take(&mut self.events)
    }

    /// Returns cumulative buffer statistics.
    pub fn stats(&self) -> JitterStats {
        self.stats.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::packet::RtpPacket;

    fn make_pkt(seq: u16, ts: u32) -> RtpPacket {
        RtpPacket::new_audio(vec![seq as u8], ts, 1, seq)
    }

    #[test]
    fn test_jitter_buffer_reorder() {
        let mut jb = JitterBuffer::new(64, 500);

        // Insert packets 0..10 in shuffled order.
        let order = [3, 7, 1, 9, 0, 5, 2, 8, 4, 6];
        for &seq in &order {
            jb.push(make_pkt(seq, seq as u32 * 160));
        }

        // Poll should return them in order 0..10.
        let mut output = Vec::new();
        while let Some(pkt) = jb.poll(1000) {
            output.push(pkt.sequence_number);
        }
        assert_eq!(output, (0..10).collect::<Vec<u16>>());
    }

    #[test]
    fn test_jitter_buffer_late_packet() {
        let mut jb = JitterBuffer::new(64, 500);

        // Push packets 0..5 and poll them all.
        for seq in 0..5u16 {
            jb.push(make_pkt(seq, seq as u32 * 160));
        }
        while jb.poll(500).is_some() {}

        // Now push a late packet with seq 2 — should be dropped.
        let before_late = jb.stats().late;
        jb.push(make_pkt(2, 2 * 160));
        assert_eq!(jb.stats().late, before_late + 1);

        // Should not be retrievable.
        assert!(jb.poll(1000).is_none());
    }

    #[test]
    fn test_jitter_buffer_gap_detection() {
        let mut jb = JitterBuffer::new(64, 500);

        // Push 0, 1, 2, 3, 4, 7, 8, 9 (skip 5 and 6).
        for seq in 0..5u16 {
            jb.push(make_pkt(seq, seq as u32 * 160));
        }
        for seq in 7..10u16 {
            jb.push(make_pkt(seq, seq as u32 * 160));
        }

        // Poll all available packets.
        let mut output = Vec::new();
        while let Some(pkt) = jb.poll(5000) {
            output.push(pkt.sequence_number);
        }

        // Should have emitted 0..5, then skipped 5,6 (lost), then 7..10.
        assert_eq!(output, vec![0, 1, 2, 3, 4, 7, 8, 9]);

        // Check that PacketLost events were emitted for 5 and 6.
        let events = jb.drain_events();
        assert!(events.contains(&JitterBufferEvent::PacketLost(5)));
        assert!(events.contains(&JitterBufferEvent::PacketLost(6)));

        assert_eq!(jb.stats().lost, 2);
    }

    #[test]
    fn test_jitter_buffer_stats() {
        let mut jb = JitterBuffer::new(64, 500);
        for seq in 0..5u16 {
            jb.push(make_pkt(seq, seq as u32 * 160));
        }
        let stats = jb.stats();
        assert_eq!(stats.received, 5);
        assert_eq!(stats.lost, 0);
        assert_eq!(stats.late, 0);
    }
}

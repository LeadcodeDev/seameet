use bytes::{BufMut, Bytes, BytesMut};
use seameet_core::SeaMeetError;

/// RTCP packet type values per RFC 3550.
const RTCP_SR: u8 = 200;
const RTCP_RR: u8 = 201;
/// RTCP transport-layer feedback (RFC 4585).
const RTCP_RTPFB: u8 = 205;
/// RTCP payload-specific feedback (RFC 4585).
const RTCP_PSFB: u8 = 206;

/// FMT value for generic NACK (RFC 4585 §6.2.1).
const NACK_FMT: u8 = 1;
/// FMT value for PLI (RFC 4585 §6.3.1).
const PLI_FMT: u8 = 1;

/// Minimal RTCP packet representations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RtcpPacket {
    /// Sender Report (PT 200). Contains the SSRC of the sender.
    SenderReport {
        /// SSRC of the sender.
        ssrc: u32,
    },
    /// Receiver Report (PT 201). Contains the SSRC of the reporter.
    ReceiverReport {
        /// SSRC of the reporter.
        ssrc: u32,
    },
    /// Generic NACK — requests retransmission of lost packets.
    Nack {
        /// SSRC of the media source.
        ssrc: u32,
        /// Sequence numbers of lost packets.
        lost_packets: Vec<u16>,
    },
    /// Picture Loss Indication — requests a keyframe.
    Pli {
        /// SSRC of the media source.
        ssrc: u32,
    },
}

impl RtcpPacket {
    /// Serializes this RTCP packet into a byte buffer.
    pub fn to_bytes(&self) -> Bytes {
        match self {
            Self::SenderReport { ssrc } => {
                let mut buf = BytesMut::with_capacity(8);
                // V=2, P=0, RC=0, PT=200
                buf.put_u8(0x80);
                buf.put_u8(RTCP_SR);
                buf.put_u16(1); // length in 32-bit words minus one
                buf.put_u32(*ssrc);
                buf.freeze()
            }
            Self::ReceiverReport { ssrc } => {
                let mut buf = BytesMut::with_capacity(8);
                buf.put_u8(0x80);
                buf.put_u8(RTCP_RR);
                buf.put_u16(1);
                buf.put_u32(*ssrc);
                buf.freeze()
            }
            Self::Nack { ssrc, lost_packets } => {
                // Each NACK FCI entry: PID (u16) + BLP (u16).
                // We encode each lost seq as its own FCI entry with BLP=0.
                let fci_count = lost_packets.len();
                // length = number of 32-bit words in the packet minus one
                // packet: header(4) + sender_ssrc(4) + media_ssrc(4) + FCI(fci_count * 4)
                // = (3 + fci_count) words => length field = 2 + fci_count
                let length = 2 + fci_count as u16;

                let mut buf = BytesMut::with_capacity(4 + (length as usize + 1) * 4);
                buf.put_u8(0x80 | NACK_FMT);
                buf.put_u8(RTCP_RTPFB);
                buf.put_u16(length);
                buf.put_u32(0); // sender SSRC (not used for NACK, set to 0)
                buf.put_u32(*ssrc); // media source SSRC

                for &seq in lost_packets {
                    buf.put_u16(seq); // PID
                    buf.put_u16(0);   // BLP (no additional losses encoded)
                }

                buf.freeze()
            }
            Self::Pli { ssrc } => {
                let mut buf = BytesMut::with_capacity(12);
                buf.put_u8(0x80 | PLI_FMT);
                buf.put_u8(RTCP_PSFB);
                buf.put_u16(2); // length: 2 words after header
                buf.put_u32(0); // sender SSRC
                buf.put_u32(*ssrc); // media source SSRC
                buf.freeze()
            }
        }
    }

    /// Parses an RTCP packet from a byte slice.
    pub fn from_bytes(buf: &[u8]) -> Result<Self, SeaMeetError> {
        if buf.len() < 8 {
            return Err(SeaMeetError::PeerConnection(
                "RTCP packet too short".into(),
            ));
        }

        let version = (buf[0] >> 6) & 0x03;
        if version != 2 {
            return Err(SeaMeetError::PeerConnection(format!(
                "unsupported RTCP version: {version}"
            )));
        }

        let fmt = buf[0] & 0x1F;
        let pt = buf[1];
        let length_words = u16::from_be_bytes([buf[2], buf[3]]) as usize;
        let expected_len = (length_words + 1) * 4;
        if buf.len() < expected_len {
            return Err(SeaMeetError::PeerConnection(
                "RTCP packet truncated".into(),
            ));
        }

        match pt {
            RTCP_SR => {
                let ssrc = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
                Ok(Self::SenderReport { ssrc })
            }
            RTCP_RR => {
                let ssrc = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
                Ok(Self::ReceiverReport { ssrc })
            }
            RTCP_RTPFB if fmt == NACK_FMT => {
                if buf.len() < 12 {
                    return Err(SeaMeetError::PeerConnection(
                        "NACK packet too short".into(),
                    ));
                }
                let ssrc = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
                let fci_bytes = &buf[12..expected_len];
                let mut lost_packets = Vec::new();
                let mut i = 0;
                while i + 3 < fci_bytes.len() + 1 && i + 1 < fci_bytes.len() {
                    let pid = u16::from_be_bytes([fci_bytes[i], fci_bytes[i + 1]]);
                    let blp = u16::from_be_bytes([fci_bytes[i + 2], fci_bytes[i + 3]]);
                    lost_packets.push(pid);
                    // Decode BLP bitmask: bit k set means pid+k+1 is also lost.
                    for bit in 0..16u16 {
                        if blp & (1 << bit) != 0 {
                            lost_packets.push(pid.wrapping_add(bit + 1));
                        }
                    }
                    i += 4;
                }
                Ok(Self::Nack { ssrc, lost_packets })
            }
            RTCP_PSFB if fmt == PLI_FMT => {
                if buf.len() < 12 {
                    return Err(SeaMeetError::PeerConnection(
                        "PLI packet too short".into(),
                    ));
                }
                let ssrc = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);
                Ok(Self::Pli { ssrc })
            }
            _ => Err(SeaMeetError::PeerConnection(format!(
                "unsupported RTCP packet type: {pt} fmt: {fmt}"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rtcp_nack_roundtrip() {
        let original = RtcpPacket::Nack {
            ssrc: 42,
            lost_packets: vec![10, 20, 30],
        };
        let bytes = original.to_bytes();
        let decoded = RtcpPacket::from_bytes(&bytes).expect("decode");
        match decoded {
            RtcpPacket::Nack { ssrc, lost_packets } => {
                assert_eq!(ssrc, 42);
                assert!(lost_packets.contains(&10));
                assert!(lost_packets.contains(&20));
                assert!(lost_packets.contains(&30));
            }
            other => panic!("expected Nack, got {other:?}"),
        }
    }

    #[test]
    fn test_sender_report_roundtrip() {
        let original = RtcpPacket::SenderReport { ssrc: 1234 };
        let bytes = original.to_bytes();
        let decoded = RtcpPacket::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_receiver_report_roundtrip() {
        let original = RtcpPacket::ReceiverReport { ssrc: 5678 };
        let bytes = original.to_bytes();
        let decoded = RtcpPacket::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_pli_roundtrip() {
        let original = RtcpPacket::Pli { ssrc: 999 };
        let bytes = original.to_bytes();
        let decoded = RtcpPacket::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_rtcp_too_short() {
        assert!(RtcpPacket::from_bytes(&[0u8; 4]).is_err());
    }
}

use bytes::{BufMut, Bytes, BytesMut};
use seameet_core::SeaMeetError;

/// RTP protocol version (always 2).
const RTP_VERSION: u8 = 2;

/// Minimum RTP header size in bytes (no CSRC, no extension).
const RTP_HEADER_SIZE: usize = 12;

/// Default payload type for audio streams.
pub const PT_AUDIO: u8 = 111;

/// Default payload type for video streams.
pub const PT_VIDEO: u8 = 96;

/// An RTP packet as defined by RFC 3550.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpPacket {
    /// RTP version (always 2).
    pub version: u8,
    /// Whether the packet contains padding at the end.
    pub padding: bool,
    /// Whether an extension header is present.
    pub extension: bool,
    /// CSRC count.
    pub cc: u8,
    /// Marker bit — signals frame boundaries for video.
    pub marker: bool,
    /// Payload type.
    pub payload_type: u8,
    /// Sequence number (wrapping u16).
    pub sequence_number: u16,
    /// Timestamp in media clock units.
    pub timestamp: u32,
    /// Synchronization source identifier.
    pub ssrc: u32,
    /// Contributing source identifiers.
    pub csrc: Vec<u32>,
    /// Payload data.
    pub payload: Vec<u8>,
}

impl RtpPacket {
    /// Creates a new audio RTP packet.
    pub fn new_audio(payload: Vec<u8>, timestamp: u32, ssrc: u32, seq: u16) -> Self {
        Self {
            version: RTP_VERSION,
            padding: false,
            extension: false,
            cc: 0,
            marker: false,
            payload_type: PT_AUDIO,
            sequence_number: seq,
            timestamp,
            ssrc,
            csrc: Vec::new(),
            payload,
        }
    }

    /// Creates a new video RTP packet.
    pub fn new_video(
        payload: Vec<u8>,
        timestamp: u32,
        ssrc: u32,
        seq: u16,
        marker: bool,
    ) -> Self {
        Self {
            version: RTP_VERSION,
            padding: false,
            extension: false,
            cc: 0,
            marker,
            payload_type: PT_VIDEO,
            sequence_number: seq,
            timestamp,
            ssrc,
            csrc: Vec::new(),
            payload,
        }
    }

    /// Serializes this packet into a byte buffer.
    pub fn to_bytes(&self) -> Bytes {
        let len = RTP_HEADER_SIZE + self.cc as usize * 4 + self.payload.len();
        let mut buf = BytesMut::with_capacity(len);

        let first_byte = (self.version << 6)
            | ((self.padding as u8) << 5)
            | ((self.extension as u8) << 4)
            | (self.cc & 0x0F);
        buf.put_u8(first_byte);

        let second_byte = ((self.marker as u8) << 7) | (self.payload_type & 0x7F);
        buf.put_u8(second_byte);

        buf.put_u16(self.sequence_number);
        buf.put_u32(self.timestamp);
        buf.put_u32(self.ssrc);

        for &csrc in &self.csrc {
            buf.put_u32(csrc);
        }

        buf.put_slice(&self.payload);
        buf.freeze()
    }

    /// Parses an RTP packet from a byte slice.
    pub fn from_bytes(buf: &[u8]) -> Result<Self, SeaMeetError> {
        if buf.len() < RTP_HEADER_SIZE {
            return Err(SeaMeetError::PeerConnection(
                "RTP packet too short".into(),
            ));
        }

        let version = (buf[0] >> 6) & 0x03;
        if version != RTP_VERSION {
            return Err(SeaMeetError::PeerConnection(format!(
                "unsupported RTP version: {version}"
            )));
        }

        let padding = (buf[0] >> 5) & 0x01 != 0;
        let extension = (buf[0] >> 4) & 0x01 != 0;
        let cc = buf[0] & 0x0F;
        let marker = (buf[1] >> 7) & 0x01 != 0;
        let payload_type = buf[1] & 0x7F;

        let sequence_number = u16::from_be_bytes([buf[2], buf[3]]);
        let timestamp = u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]);
        let ssrc = u32::from_be_bytes([buf[8], buf[9], buf[10], buf[11]]);

        let csrc_end = RTP_HEADER_SIZE + cc as usize * 4;
        if buf.len() < csrc_end {
            return Err(SeaMeetError::PeerConnection(
                "RTP packet too short for CSRC list".into(),
            ));
        }

        let mut csrc = Vec::with_capacity(cc as usize);
        for i in 0..cc as usize {
            let offset = RTP_HEADER_SIZE + i * 4;
            csrc.push(u32::from_be_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            ]));
        }

        let payload = buf[csrc_end..].to_vec();

        Ok(Self {
            version,
            padding,
            extension,
            cc,
            marker,
            payload_type,
            sequence_number,
            timestamp,
            ssrc,
            csrc,
            payload,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rtp_packet_roundtrip() {
        let original = RtpPacket::new_audio(vec![0xDE, 0xAD, 0xBE, 0xEF], 160, 12345, 42);
        let bytes = original.to_bytes();
        let decoded = RtpPacket::from_bytes(&bytes).expect("decode");

        assert_eq!(decoded.version, 2);
        assert_eq!(decoded.padding, false);
        assert_eq!(decoded.extension, false);
        assert_eq!(decoded.cc, 0);
        assert_eq!(decoded.marker, false);
        assert_eq!(decoded.payload_type, PT_AUDIO);
        assert_eq!(decoded.sequence_number, 42);
        assert_eq!(decoded.timestamp, 160);
        assert_eq!(decoded.ssrc, 12345);
        assert_eq!(decoded.payload, vec![0xDE, 0xAD, 0xBE, 0xEF]);
        assert_eq!(decoded, original);
    }

    #[test]
    fn test_video_packet_marker() {
        let pkt = RtpPacket::new_video(vec![1, 2, 3], 3000, 999, 1, true);
        let bytes = pkt.to_bytes();
        let decoded = RtpPacket::from_bytes(&bytes).expect("decode");
        assert!(decoded.marker);
        assert_eq!(decoded.payload_type, PT_VIDEO);
    }

    #[test]
    fn test_from_bytes_too_short() {
        let result = RtpPacket::from_bytes(&[0u8; 4]);
        assert!(result.is_err());
    }

    #[test]
    fn test_from_bytes_bad_version() {
        let mut buf = [0u8; 12];
        buf[0] = 0b0100_0000; // version 1
        let result = RtpPacket::from_bytes(&buf);
        assert!(result.is_err());
    }

    #[test]
    fn test_csrc_roundtrip() {
        let mut pkt = RtpPacket::new_audio(vec![0xFF], 320, 1, 10);
        pkt.cc = 2;
        pkt.csrc = vec![100, 200];
        let bytes = pkt.to_bytes();
        let decoded = RtpPacket::from_bytes(&bytes).expect("decode");
        assert_eq!(decoded.csrc, vec![100, 200]);
        assert_eq!(decoded.cc, 2);
        assert_eq!(decoded.payload, vec![0xFF]);
    }
}

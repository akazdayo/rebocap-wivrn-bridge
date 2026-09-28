//! Subset of rebocap_pb.RebocapMsg documented by rebocap-linux-compat.
use prost::{Message, Oneof};
use std::io::{self, Read, Write};

pub const MAX_PAYLOAD: usize = 2044;

#[derive(Clone, PartialEq, Message)]
pub struct MessageEnvelope {
    #[prost(oneof = "message_envelope::Body", tags = "1, 2, 3, 4")]
    pub body: Option<message_envelope::Body>,
}

pub mod message_envelope {
    use super::*;

    #[derive(Clone, PartialEq, Oneof)]
    pub enum Body {
        #[prost(message, tag = "1")]
        Position(super::Position),
        #[prost(message, tag = "2")]
        TrackerAdded(super::DeviceNew),
        #[prost(message, tag = "3")]
        TrackerStatus(super::DeviceStatus),
        #[prost(message, tag = "4")]
        UniverseChange(super::UniverseChange),
    }
}

#[derive(Clone, PartialEq, Message)]
pub struct Position {
    #[prost(int32, tag = "1")]
    pub tracker_id: i32,
    // Rebocap uses [w, x, y, z] and packed repeated doubles/floats.
    #[prost(double, repeated, tag = "10")]
    pub q: Vec<f64>,
    #[prost(float, repeated, tag = "11")]
    pub pos: Vec<f32>,
}

#[derive(Clone, PartialEq, Message)]
pub struct DeviceNew {
    #[prost(int32, tag = "1")]
    pub tracker_id: i32,
    #[prost(int32, tag = "3")]
    pub tracker_role: i32,
    #[prost(string, tag = "4")]
    pub tracker_name: String,
}

#[derive(Clone, PartialEq, Message)]
pub struct DeviceStatus {
    #[prost(int32, tag = "1")]
    pub tracker_id: i32,
    #[prost(int32, tag = "2")]
    pub status: i32,
    #[prost(float, tag = "3")]
    pub battery_level: f32,
}

#[derive(Clone, PartialEq, Message)]
pub struct UniverseChange {
    #[prost(float, tag = "1")]
    pub yaw: f32,
    #[prost(float, repeated, tag = "2")]
    pub pos: Vec<f32>,
}

pub fn send(writer: &mut impl Write, body: message_envelope::Body) -> io::Result<()> {
    let bytes = MessageEnvelope { body: Some(body) }.encode_to_vec();
    if bytes.len() > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Rebocap message too long",
        ));
    }
    writer.write_all(&(bytes.len() as u32).to_le_bytes())?;
    writer.write_all(&bytes)
}

pub fn receive(reader: &mut impl Read) -> io::Result<MessageEnvelope> {
    let mut size = [0; 4];
    reader.read_exact(&mut size)?;
    let len = u32::from_le_bytes(size) as usize;
    if len > MAX_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "oversized Rebocap frame",
        ));
    }
    let mut bytes = vec![0; len];
    reader.read_exact(&mut bytes)?;
    MessageEnvelope::decode(&bytes[..]).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_and_quaternion_order() {
        let body = message_envelope::Body::Position(Position {
            tracker_id: 0,
            q: vec![1.0, 0.0, 0.0, 0.0],
            pos: vec![0.5, 1.7, -0.3],
        });
        let mut buf = Vec::new();
        send(&mut buf, body.clone()).unwrap();
        assert_eq!(
            u32::from_le_bytes(buf[..4].try_into().unwrap()) as usize,
            buf.len() - 4
        );
        assert_eq!(receive(&mut &buf[..]).unwrap().body, Some(body));
    }

    #[test]
    fn rejects_oversized_frames_before_allocating() {
        let bad = (MAX_PAYLOAD as u32 + 1).to_le_bytes();
        assert_eq!(
            receive(&mut &bad[..]).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn rebocap_handshake_matches_wire_fields() {
        let mut status = Vec::new();
        send(
            &mut status,
            message_envelope::Body::TrackerStatus(DeviceStatus {
                tracker_id: 0,
                status: 1,
                battery_level: 0.0,
            }),
        )
        .unwrap();
        assert_eq!(status, [4, 0, 0, 0, 0x1a, 2, 0x10, 1]);

        let mut universe = Vec::new();
        send(
            &mut universe,
            message_envelope::Body::UniverseChange(UniverseChange {
                yaw: 0.0,
                pos: vec![0.0; 3],
            }),
        )
        .unwrap();
        assert_eq!(&universe[..8], &[16, 0, 0, 0, 0x22, 14, 0x12, 12]);
        assert!(universe[8..].iter().all(|byte| *byte == 0));
    }
}

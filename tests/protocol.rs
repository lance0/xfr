//! Protocol parsing tests

use xfr::protocol::{
    AggregateInterval, ControlMessage, Direction, Protocol, StreamInterval, StreamResult,
    TcpInfoSnapshot, TestResult,
};

#[test]
fn test_hello_serialization() {
    let msg = ControlMessage::client_hello();
    let json = msg.serialize().unwrap();

    assert!(json.contains("\"type\":\"hello\""));
    assert!(json.contains("\"version\":\"1.1\""));
    assert!(json.contains("\"client\":\"xfr/"));
}

#[test]
fn test_server_hello_serialization() {
    let msg = ControlMessage::server_hello();
    let json = msg.serialize().unwrap();

    assert!(json.contains("\"type\":\"hello\""));
    assert!(json.contains("\"server\":\"xfr/"));
    assert!(json.contains("\"capabilities\""));
}

#[test]
fn test_test_start_roundtrip() {
    let msg = ControlMessage::TestStart {
        id: "test-123".to_string(),
        protocol: Protocol::Tcp,
        streams: 4,
        duration_secs: 30,
        direction: Direction::Upload,
        bitrate: None,
    };

    let json = msg.serialize().unwrap();
    let decoded = ControlMessage::deserialize(&json).unwrap();

    match decoded {
        ControlMessage::TestStart {
            id,
            protocol,
            streams,
            duration_secs,
            direction,
            bitrate,
        } => {
            assert_eq!(id, "test-123");
            assert_eq!(protocol, Protocol::Tcp);
            assert_eq!(streams, 4);
            assert_eq!(duration_secs, 30);
            assert_eq!(direction, Direction::Upload);
            assert!(bitrate.is_none());
        }
        _ => panic!("Wrong message type"),
    }
}

#[test]
fn test_udp_test_start() {
    let msg = ControlMessage::TestStart {
        id: "udp-test".to_string(),
        protocol: Protocol::Udp,
        streams: 1,
        duration_secs: 10,
        direction: Direction::Download,
        bitrate: Some(1_000_000_000),
    };

    let json = msg.serialize().unwrap();
    assert!(json.contains("\"protocol\":\"udp\""));
    assert!(json.contains("\"bitrate\":1000000000"));
    assert!(json.contains("\"direction\":\"download\""));
}

#[test]
fn test_interval_message() {
    let msg = ControlMessage::Interval {
        id: "test-123".to_string(),
        elapsed_ms: 1000,
        streams: vec![
            StreamInterval {
                id: 0,
                bytes: 125_000_000,
                retransmits: Some(2),
                jitter_ms: None,
                lost: None,
                error: None,
            },
            StreamInterval {
                id: 1,
                bytes: 125_000_000,
                retransmits: Some(1),
                jitter_ms: None,
                lost: None,
                error: None,
            },
        ],
        aggregate: AggregateInterval {
            bytes: 250_000_000,
            throughput_mbps: 2000.0,
            retransmits: Some(3),
            jitter_ms: None,
            lost: None,
        },
    };

    let json = msg.serialize().unwrap();
    let decoded = ControlMessage::deserialize(&json).unwrap();

    match decoded {
        ControlMessage::Interval {
            elapsed_ms,
            streams,
            aggregate,
            ..
        } => {
            assert_eq!(elapsed_ms, 1000);
            assert_eq!(streams.len(), 2);
            assert_eq!(aggregate.throughput_mbps, 2000.0);
        }
        _ => panic!("Wrong message type"),
    }
}

#[test]
fn test_result_message() {
    let result = TestResult {
        id: "test-123".to_string(),
        bytes_total: 1_250_000_000,
        duration_ms: 10_000,
        throughput_mbps: 1000.0,
        streams: vec![StreamResult {
            id: 0,
            bytes: 1_250_000_000,
            throughput_mbps: 1000.0,
            retransmits: Some(42),
            jitter_ms: None,
            lost: None,
        }],
        tcp_info: Some(TcpInfoSnapshot {
            retransmits: 42,
            rtt_us: 1234,
            rtt_var_us: 100,
            cwnd: 65535,
        }),
        udp_stats: None,
    };

    let msg = ControlMessage::Result(result);
    let json = msg.serialize().unwrap();

    assert!(json.contains("\"bytes_total\":1250000000"));
    assert!(json.contains("\"throughput_mbps\":1000.0"));
    assert!(json.contains("\"rtt_us\":1234"));
}

#[test]
fn test_error_message() {
    let msg = ControlMessage::error("Port already in use");
    let json = msg.serialize().unwrap();

    assert!(json.contains("\"type\":\"error\""));
    assert!(json.contains("\"message\":\"Port already in use\""));
}

#[test]
fn test_cancel_message() {
    let msg = ControlMessage::Cancel {
        id: "test-123".to_string(),
        reason: "User requested".to_string(),
    };

    let json = msg.serialize().unwrap();
    let decoded = ControlMessage::deserialize(&json).unwrap();

    match decoded {
        ControlMessage::Cancel { id, reason } => {
            assert_eq!(id, "test-123");
            assert_eq!(reason, "User requested");
        }
        _ => panic!("Wrong message type"),
    }
}

#[test]
fn test_protocol_display() {
    assert_eq!(format!("{}", Protocol::Tcp), "TCP");
    assert_eq!(format!("{}", Protocol::Udp), "UDP");
}

#[test]
fn test_direction_display() {
    assert_eq!(format!("{}", Direction::Upload), "Upload");
    assert_eq!(format!("{}", Direction::Download), "Download");
    assert_eq!(format!("{}", Direction::Bidir), "Bidirectional");
}

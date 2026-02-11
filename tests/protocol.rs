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
    assert!(json.contains("\"capabilities\""));
    assert!(json.contains("\"single_port_tcp\""));
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
        congestion: Some("bbr".to_string()),
    };

    let json = msg.serialize().unwrap();
    assert!(json.contains("\"congestion\":\"bbr\""));
    let decoded = ControlMessage::deserialize(&json).unwrap();

    match decoded {
        ControlMessage::TestStart {
            id,
            protocol,
            streams,
            duration_secs,
            direction,
            bitrate,
            congestion,
        } => {
            assert_eq!(id, "test-123");
            assert_eq!(protocol, Protocol::Tcp);
            assert_eq!(streams, 4);
            assert_eq!(duration_secs, 30);
            assert_eq!(direction, Direction::Upload);
            assert!(bitrate.is_none());
            assert_eq!(congestion, Some("bbr".to_string()));
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
        congestion: None,
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
                rtt_us: Some(1234),
                cwnd: Some(65535),
            },
            StreamInterval {
                id: 1,
                bytes: 125_000_000,
                retransmits: Some(1),
                jitter_ms: None,
                lost: None,
                error: None,
                rtt_us: Some(1200),
                cwnd: Some(32768),
            },
        ],
        aggregate: AggregateInterval {
            bytes: 250_000_000,
            throughput_mbps: 2000.0,
            retransmits: Some(3),
            jitter_ms: None,
            lost: None,
            rtt_us: Some(1217),
            cwnd: Some(98303),
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
            assert_eq!(aggregate.rtt_us, Some(1217));
            assert_eq!(aggregate.cwnd, Some(98303));
            assert_eq!(streams[0].rtt_us, Some(1234));
            assert_eq!(streams[1].cwnd, Some(32768));
        }
        _ => panic!("Wrong message type"),
    }
}

#[test]
fn test_interval_forward_compat() {
    // Interval JSON with extra unknown fields should still deserialize (serde ignores them)
    let json = r#"{"type":"interval","id":"test-1","elapsed_ms":1000,"streams":[{"id":0,"bytes":100,"retransmits":0,"rtt_us":500,"cwnd":1024,"future_field":99}],"aggregate":{"bytes":100,"throughput_mbps":0.8,"retransmits":0,"rtt_us":500,"cwnd":1024,"future_field":99}}"#;
    let decoded = ControlMessage::deserialize(json).unwrap();
    match decoded {
        ControlMessage::Interval {
            streams, aggregate, ..
        } => {
            assert_eq!(streams[0].rtt_us, Some(500));
            assert_eq!(streams[0].cwnd, Some(1024));
            assert_eq!(aggregate.rtt_us, Some(500));
            assert_eq!(aggregate.cwnd, Some(1024));
        }
        _ => panic!("Expected Interval"),
    }
}

#[test]
fn test_interval_backward_compat() {
    // Interval JSON without rtt_us/cwnd should deserialize with None
    let json = r#"{"type":"interval","id":"test-1","elapsed_ms":1000,"streams":[{"id":0,"bytes":100,"retransmits":0}],"aggregate":{"bytes":100,"throughput_mbps":0.8,"retransmits":0}}"#;
    let decoded = ControlMessage::deserialize(json).unwrap();
    match decoded {
        ControlMessage::Interval {
            streams, aggregate, ..
        } => {
            assert!(streams[0].rtt_us.is_none());
            assert!(streams[0].cwnd.is_none());
            assert!(aggregate.rtt_us.is_none());
            assert!(aggregate.cwnd.is_none());
        }
        _ => panic!("Expected Interval"),
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

#[test]
fn test_data_hello_serialization() {
    let msg = ControlMessage::DataHello {
        test_id: "test-123".to_string(),
        stream_index: 0,
    };
    let json = msg.serialize().unwrap();

    assert!(json.contains("\"type\":\"data_hello\""));
    assert!(json.contains("\"test_id\":\"test-123\""));

    // Test roundtrip
    let decoded = ControlMessage::deserialize(&json).unwrap();
    match decoded {
        ControlMessage::DataHello {
            test_id,
            stream_index,
        } => {
            assert_eq!(test_id, "test-123");
            assert_eq!(stream_index, 0);
        }
        _ => panic!("Expected DataHello"),
    }
}

#[test]
fn test_pause_message_roundtrip() {
    let msg = ControlMessage::Pause {
        id: "test-456".to_string(),
    };
    let json = msg.serialize().unwrap();
    let decoded = ControlMessage::deserialize(&json).unwrap();

    match decoded {
        ControlMessage::Pause { id } => {
            assert_eq!(id, "test-456");
        }
        _ => panic!("Expected Pause"),
    }
}

#[test]
fn test_resume_message_roundtrip() {
    let msg = ControlMessage::Resume {
        id: "test-789".to_string(),
    };
    let json = msg.serialize().unwrap();
    let decoded = ControlMessage::deserialize(&json).unwrap();

    match decoded {
        ControlMessage::Resume { id } => {
            assert_eq!(id, "test-789");
        }
        _ => panic!("Expected Resume"),
    }
}

#[test]
fn test_pause_resume_capability_in_hello() {
    let client = ControlMessage::client_hello();
    let client_json = client.serialize().unwrap();
    assert!(
        client_json.contains("\"pause_resume\""),
        "client_hello should advertise pause_resume capability"
    );

    let server = ControlMessage::server_hello();
    let server_json = server.serialize().unwrap();
    assert!(
        server_json.contains("\"pause_resume\""),
        "server_hello should advertise pause_resume capability"
    );
}

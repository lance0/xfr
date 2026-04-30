//! End-to-end coverage of the live UDP loss data path that the unit tests
//! only exercise in slices: server `StreamStats` → `AggregateInterval` →
//! `ControlMessage::Interval` → `TestProgress` → `App.on_progress`.
//!
//! The unit tests in `src/protocol.rs`, `src/tui/app.rs`, and
//! `src/tui/widgets.rs` each cover their layer in isolation. This file
//! glues them together so a regression in any plumbing step (forgotten
//! field on a struct ripple, server-side delta math, client decode) shows
//! up as a single failing assertion against the rendered app state.

use xfr::client::TestProgress;
use xfr::protocol::{AggregateInterval, ControlMessage};
use xfr::stats::TestStats;
use xfr::tui::app::{App, AppState};

#[test]
fn live_loss_path_carries_through_to_app_state() {
    // Two-stream UDP test, two periodic intervals. The first interval
    // baselines `prev_udp_progress`; the second produces the first real
    // sparkline-severity reading. We assert the rendered `App` state
    // matches the cumulative loss percent the server would have seen,
    // confirming every link in the chain is wired correctly.

    let stats = TestStats::new("live-path".to_string(), 2);

    // Interval 1: 600 received total, 0 lost.
    stats.streams[0].add_udp_received(300);
    stats.streams[1].add_udp_received(300);
    let intervals_1: Vec<_> = stats.streams.iter().map(|s| s.record_interval()).collect();
    let agg_1 = stats.to_aggregate_with_direction(&intervals_1, false);
    let progress_1 = roundtrip_via_wire(&agg_1);

    let mut app = App::default();
    app.state = AppState::Running;
    app.on_progress(progress_1);

    // After the first interval we should have a real cumulative
    // udp_progress, but the sparkline sample baselines (no severity tint
    // because there's no prior point to delta against). Cumulative is
    // 0/600 = 0.0%, so the live counter is fresh 0.0%, not unknown.
    assert_eq!(app.udp_lost_percent, Some(0.0));
    let baseline = app.throughput_history.back().copied().unwrap();
    assert_eq!(baseline.interval_packets, 0);
    assert_eq!(baseline.loss_rate_percent(), None);

    // Interval 2: +400 received, +100 lost across the two streams. So
    // cumulative is 100/(1000+100) ≈ 9.09%, and the per-interval delta
    // is 100 lost / 500 packets in window = 20% — heavy, error-tinted.
    stats.streams[0].add_udp_received(200);
    stats.streams[0].add_udp_lost(40);
    stats.streams[1].add_udp_received(200);
    stats.streams[1].add_udp_lost(60);
    let intervals_2: Vec<_> = stats.streams.iter().map(|s| s.record_interval()).collect();
    let agg_2 = stats.to_aggregate_with_direction(&intervals_2, false);
    let progress_2 = roundtrip_via_wire(&agg_2);

    app.on_progress(progress_2);

    // Cumulative loss: 100 / (1000 + 100) = 9.090909…%.
    let cumulative = app.udp_lost_percent.expect("expected fresh cumulative %");
    assert!(
        (cumulative - (100.0 / 1100.0 * 100.0)).abs() < 1e-9,
        "expected ~9.09% cumulative, got {cumulative}",
    );

    // Per-interval rate (from sparkline sample): 100 / 500 = 20%.
    let lossy = app.throughput_history.back().copied().unwrap();
    assert_eq!(lossy.lost_packets, 100);
    assert_eq!(lossy.interval_packets, 500);
    assert_eq!(lossy.loss_rate_percent(), Some(20.0));
}

#[test]
fn live_loss_path_pre_0_9_11_server_keeps_unknown() {
    // Hand-roll an `AggregateInterval` without `udp_progress` (mirrors
    // what a pre-0.9.11 server would put on the wire). The whole-test
    // contract: TUI must stay at unknown loss % rather than report a
    // misleading 0.0% next to actual loss bursts the user can see.
    let agg = AggregateInterval {
        bytes: 1_000,
        throughput_mbps: 8.0,
        retransmits: None,
        jitter_ms: Some(0.4),
        lost: Some(5),
        udp_progress: None,
        rtt_us: None,
        cwnd: None,
        bytes_sent: None,
        bytes_received: None,
        throughput_send_mbps: None,
        throughput_recv_mbps: None,
    };

    let mut app = App::default();
    app.state = AppState::Running;
    app.on_progress(roundtrip_via_wire(&agg));
    app.on_progress(roundtrip_via_wire(&agg));

    assert_eq!(
        app.udp_lost_percent, None,
        "no udp_progress means unknown for the whole run, not stale 0%",
    );
}

/// Serialize an `AggregateInterval` to JSON, deserialize via the actual
/// `ControlMessage::Interval` path, and reconstruct the `TestProgress` the
/// client would have built. Catches any field that would silently drop on
/// the wire round trip.
fn roundtrip_via_wire(agg: &AggregateInterval) -> TestProgress {
    // Build the same wrapper the server would emit so deserialization
    // exercises the union-tag dispatch alongside the field shape.
    let json = format!(
        "{{\"type\":\"interval\",\"id\":\"live-path\",\"elapsed_ms\":1000,\"streams\":[],\"aggregate\":{}}}",
        serde_json::to_string(agg).unwrap()
    );
    let decoded = ControlMessage::deserialize(&json).expect("interval roundtrip");
    let ControlMessage::Interval {
        elapsed_ms,
        streams,
        aggregate,
        ..
    } = decoded
    else {
        panic!("expected Interval");
    };

    TestProgress {
        elapsed_ms,
        total_bytes: aggregate.bytes,
        throughput_mbps: aggregate.throughput_mbps,
        streams,
        rtt_us: aggregate.rtt_us,
        cwnd: aggregate.cwnd,
        total_retransmits: None,
        bytes_sent: aggregate.bytes_sent,
        bytes_received: aggregate.bytes_received,
        throughput_send_mbps: aggregate.throughput_send_mbps,
        throughput_recv_mbps: aggregate.throughput_recv_mbps,
        udp_progress: aggregate.udp_progress,
    }
}

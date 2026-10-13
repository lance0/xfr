//! Exercise receiver truth and legacy interoperability through Client::run.
//! The peer allocates ephemeral TCP/UDP ports and drops scripted upload
//! sequences, so asymmetric loss does not depend on overflowing socket buffers.

use std::net::SocketAddr;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, UdpSocket, tcp::OwnedWriteHalf};
use tokio::sync::{mpsc, oneshot};
use tokio::time::timeout;

use xfr::client::{Client, ClientConfig, TestProgress, ZerocopyMode};
use xfr::protocol::{ControlMessage, Direction, PROTOCOL_VERSION, Protocol, TestResult, UdpStats};
use xfr::udp::{PacketTracker, UDP_PAYLOAD_SIZE, UdpPacketHeader};

const WAIT: Duration = Duration::from_secs(5);
const INITIAL_DOWNLOAD_BYTES: [u64; 2] = [4 * 64, 4 * 96];
const FINAL_DOWNLOAD_BYTES: [u64; 2] = [4 * 64 + 32, 4 * 96 + 48];
const REPORTED_DOWNLOAD_BYTES: u64 = 900_000;

#[derive(Clone, Copy)]
enum PeerVersion {
    Directional,
    LegacyStreams,
    LegacyWithoutSplits,
}

impl PeerVersion {
    fn has_aggregate_split(self) -> bool {
        !matches!(self, Self::LegacyWithoutSplits)
    }
}

enum PeerCommand {
    Interval,
    Tail,
    Result,
}

struct UploadReading {
    address: SocketAddr,
    bytes: u64,
    received: u64,
    lost: u64,
}

struct PeerFacts {
    upload_bytes: [u64; 2],
    upload_stats: UdpStats,
}

struct Fixture {
    client: tokio::task::JoinHandle<anyhow::Result<TestResult>>,
    peer: tokio::task::JoinHandle<()>,
    commands: mpsc::Sender<PeerCommand>,
    progress: mpsc::Receiver<TestProgress>,
    facts: PeerFacts,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.client.abort();
        self.peer.abort();
    }
}

async fn write_json(writer: &mut OwnedWriteHalf, message: &Value) {
    writer
        .write_all(format!("{message}\n").as_bytes())
        .await
        .unwrap();
}

async fn send_packet(
    socket: &UdpSocket,
    address: SocketAddr,
    sequence: u64,
    ordinal: u64,
    size: usize,
) {
    let mut packet = vec![0x5a; size];
    assert!(
        UdpPacketHeader {
            sequence,
            // Deliberate sender timestamp variation ensures the jitter path
            // is exercised; socket scheduling makes its exact value variable.
            timestamp_us: ordinal * 1_000_000,
        }
        .encode(&mut packet)
    );
    assert_eq!(socket.send_to(&packet, address).await.unwrap(), size);
}

async fn collect_upload(socket: &UdpSocket, stream: usize) -> UploadReading {
    let kept = if stream == 0 {
        [0, 1, 4, 5]
    } else {
        [0, 2, 3, 7]
    };
    let download = if stream == 0 {
        [0, 3, 4, 9]
    } else {
        [0, 1, 4, 5]
    };
    let download_size = if stream == 0 { 64 } else { 96 };
    let mut tracker = PacketTracker::new();
    let mut bytes = 0;
    let mut received = 0;
    let mut address = None;
    let mut packet = vec![0; UDP_PAYLOAD_SIZE];
    for expected in 0..8 {
        let (size, source) = timeout(WAIT, socket.recv_from(&mut packet))
            .await
            .expect("production client must send its upload packets")
            .unwrap();
        let header = UdpPacketHeader::decode(&packet[..size]).unwrap();
        assert_eq!(
            header.sequence, expected,
            "low-rate loopback must stay ordered"
        );
        if let Some(address) = address {
            assert_eq!(address, source);
        } else {
            address = Some(source);
            for (ordinal, sequence) in download.into_iter().enumerate() {
                send_packet(socket, source, sequence, ordinal as u64, download_size).await;
            }
        }
        // These packets arrived at our injector, but only the kept sequences
        // reach the simulated upload receiver. Its tracker sees actual client
        // sequence numbers, independently of the download sequence space.
        if kept.contains(&header.sequence) {
            tracker.record(header.sequence);
            bytes += size as u64;
            received += 1;
        }
    }
    let (lost, _, _) = tracker.stats(received);
    UploadReading {
        address: address.unwrap(),
        bytes,
        received,
        lost,
    }
}

fn stream_row(id: usize, upload: u64, version: PeerVersion, final_result: bool) -> Value {
    let duration_secs = if final_result { 2.0 } else { 1.0 };
    let mut row = json!({
        "id": id,
        "bytes": upload + REPORTED_DOWNLOAD_BYTES,
        "jitter_ms": 1.25,
        "lost": if id == 0 { 2 } else { 4 },
    });
    if final_result {
        row["throughput_mbps"] =
            json!((upload + REPORTED_DOWNLOAD_BYTES) as f64 * 8.0 / duration_secs / 1_000_000.0);
    }
    if matches!(version, PeerVersion::Directional) {
        row["bytes_sent"] = json!(upload);
        row["bytes_received"] = json!(REPORTED_DOWNLOAD_BYTES);
        row["throughput_send_mbps"] = json!(upload as f64 * 8.0 / duration_secs / 1_000_000.0);
        row["throughput_recv_mbps"] =
            json!(REPORTED_DOWNLOAD_BYTES as f64 * 8.0 / duration_secs / 1_000_000.0);
    }
    row
}

async fn fixture(version: PeerVersion) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let sockets = [
        UdpSocket::bind("127.0.0.1:0").await.unwrap(),
        UdpSocket::bind("127.0.0.1:0").await.unwrap(),
    ];
    let data_ports: Vec<_> = sockets
        .iter()
        .map(|socket| socket.local_addr().unwrap().port())
        .collect();
    let (commands, mut command_rx) = mpsc::channel(4);
    let (facts_tx, facts_rx) = oneshot::channel();
    let peer = tokio::spawn(async move {
        let (control, _) = listener.accept().await.unwrap();
        let (reader, mut writer) = control.into_split();
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(matches!(
            ControlMessage::deserialize(line.trim()).unwrap(),
            ControlMessage::Hello { .. }
        ));
        // No single-port UDP capability: each data stream connects to the
        // explicit ephemeral port in TestAck, as an older server would require.
        let protocol_version = if matches!(version, PeerVersion::Directional) {
            PROTOCOL_VERSION
        } else {
            "1.0"
        };
        write_json(
            &mut writer,
            &json!({
                "type":"hello", "version":protocol_version,
                "server":"directional-fixture", "capabilities":[],
            }),
        )
        .await;
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        let ControlMessage::TestStart {
            id,
            protocol,
            direction,
            streams,
            ..
        } = ControlMessage::deserialize(line.trim()).unwrap()
        else {
            panic!("expected TestStart")
        };
        assert_eq!(protocol, Protocol::Udp);
        assert_eq!(direction, Direction::Bidir);
        assert_eq!(streams, 2);
        write_json(
            &mut writer,
            &json!({"type":"test_ack", "id":id, "data_ports":data_ports}),
        )
        .await;

        let (first, second) = tokio::join!(
            collect_upload(&sockets[0], 0),
            collect_upload(&sockets[1], 1)
        );
        let uploads = [first, second];
        let upload_bytes = uploads.each_ref().map(|reading| reading.bytes);
        let received = uploads.iter().map(|reading| reading.received).sum::<u64>();
        let lost = uploads.iter().map(|reading| reading.lost).sum::<u64>();
        let upload_stats = UdpStats {
            packets_sent: received + lost,
            packets_received: received,
            lost,
            lost_percent: lost as f64 / (received + lost) as f64 * 100.0,
            jitter_ms: 1.25,
            out_of_order: 0,
            jitter_max_ms: Some(2.5),
            packet_size: Some(UDP_PAYLOAD_SIZE as u32),
        };
        facts_tx
            .send(PeerFacts {
                upload_bytes,
                upload_stats: upload_stats.clone(),
            })
            .unwrap_or_else(|_| panic!("fixture facts receiver closed"));

        while let Some(command) = command_rx.recv().await {
            match command {
                PeerCommand::Interval => {
                    let total_upload = upload_bytes.iter().sum::<u64>();
                    let mut aggregate = json!({
                        "bytes": total_upload + 2 * REPORTED_DOWNLOAD_BYTES,
                        "throughput_mbps": (total_upload + 2 * REPORTED_DOWNLOAD_BYTES) as f64 * 8.0 / 1_000_000.0,
                        "udp_progress": {"packets_received":received, "packets_lost":lost},
                        "jitter_ms":1.25, "lost":lost,
                    });
                    if version.has_aggregate_split() {
                        aggregate["bytes_sent"] = json!(total_upload);
                        aggregate["bytes_received"] = json!(2 * REPORTED_DOWNLOAD_BYTES);
                        aggregate["throughput_send_mbps"] =
                            json!(total_upload as f64 * 8.0 / 1_000_000.0);
                        aggregate["throughput_recv_mbps"] =
                            json!((2 * REPORTED_DOWNLOAD_BYTES) as f64 * 8.0 / 1_000_000.0);
                    } else {
                        // This peer predates cumulative live loss too. A
                        // jitter reading alone cannot make upload loss known.
                        aggregate.as_object_mut().unwrap().remove("udp_progress");
                    }
                    // Deliberately reverse rows: local overlays must key by
                    // stream ID, rather than zip with receiver-vector order.
                    write_json(&mut writer, &json!({
                        "type":"interval", "id":id, "elapsed_ms":1000,
                        "streams":[stream_row(1,upload_bytes[1],version,false),stream_row(0,upload_bytes[0],version,false)],
                        "aggregate":aggregate,
                    })).await;
                }
                PeerCommand::Tail => {
                    // These arrive after the last server Interval. The test
                    // waits until local progress confirms reception before
                    // requesting Result, so shutdown cannot race UDP arrival.
                    send_packet(&sockets[0], uploads[0].address, 10, 4, 32).await;
                    send_packet(&sockets[1], uploads[1].address, 6, 4, 48).await;
                }
                PeerCommand::Result => {
                    let total_upload = upload_bytes.iter().sum::<u64>();
                    let mut result = json!({
                        "type":"result", "id":id, "duration_ms":2000,
                        "bytes_total":total_upload + 2 * REPORTED_DOWNLOAD_BYTES,
                        "throughput_mbps":(total_upload + 2 * REPORTED_DOWNLOAD_BYTES) as f64 * 4.0 / 1_000_000.0,
                        "streams":[stream_row(1,upload_bytes[1],version,true),stream_row(0,upload_bytes[0],version,true)],
                        "udp_stats":upload_stats,
                    });
                    if version.has_aggregate_split() {
                        result["bytes_sent"] = json!(total_upload);
                        result["bytes_received"] = json!(2 * REPORTED_DOWNLOAD_BYTES);
                        result["throughput_send_mbps"] =
                            json!(total_upload as f64 * 4.0 / 1_000_000.0);
                        result["throughput_recv_mbps"] =
                            json!((2 * REPORTED_DOWNLOAD_BYTES) as f64 * 4.0 / 1_000_000.0);
                    }
                    if matches!(version, PeerVersion::Directional) {
                        result["udp_stats_send"] = json!(upload_stats);
                    }
                    write_json(&mut writer, &result).await;
                    break;
                }
            }
        }
    });
    let (progress_tx, progress) = mpsc::channel(16);
    let client = tokio::spawn(async move {
        Client::new(ClientConfig {
            host: "127.0.0.1".to_owned(),
            port,
            protocol: Protocol::Udp,
            direction: Direction::Bidir,
            streams: 2,
            duration: Duration::from_secs(10),
            // Ten packets/second per stream: no incidental loopback loss.
            bitrate: Some(2 * UDP_PAYLOAD_SIZE as u64 * 8 * 10),
            zerocopy: ZerocopyMode::Off,
            ..Default::default()
        })
        .run(Some(progress_tx))
        .await
    });
    let facts = timeout(WAIT, facts_rx)
        .await
        .expect("peer must initialize")
        .unwrap();
    Fixture {
        client,
        peer,
        commands,
        progress,
        facts,
    }
}

async fn wait_progress(
    receiver: &mut mpsc::Receiver<TestProgress>,
    matches: impl Fn(&TestProgress) -> bool,
) -> TestProgress {
    timeout(WAIT, async {
        while let Some(progress) = receiver.recv().await {
            if matches(&progress) {
                return progress;
            }
        }
        panic!("client closed progress before the expected reading");
    })
    .await
    .expect("expected live directional progress")
}

fn assert_jitter(jitter: Option<f64>) {
    let jitter = jitter.expect("receiver must report measured jitter");
    assert!(jitter.is_finite() && jitter >= 0.0);
}

async fn exercise(version: PeerVersion) {
    let mut fixture = fixture(version).await;
    let initial = wait_progress(&mut fixture.progress, |progress| {
        progress
            .udp_recv
            .as_ref()
            .is_some_and(|recv| recv.progress.packets_received == 8)
    })
    .await;
    let recv = initial.udp_recv.unwrap();
    assert_eq!(recv.progress.packets_lost, 8);
    assert_jitter(recv.jitter_ms);
    assert!(
        initial.udp_send.is_none(),
        "no TCP report means upload is unknown"
    );
    assert!(
        initial.udp_progress.is_none(),
        "local download must not populate legacy upload loss"
    );
    assert!(initial.udp_feedback_only);

    fixture.commands.send(PeerCommand::Interval).await.unwrap();
    let reported = wait_progress(&mut fixture.progress, |progress| {
        !progress.udp_feedback_only && !progress.streams.is_empty()
    })
    .await;
    if version.has_aggregate_split() {
        let send = reported.udp_send.unwrap();
        assert_eq!(send.progress.packets_received, 8);
        assert_eq!(send.progress.packets_lost, 6);
        assert_eq!(send.jitter_ms, Some(1.25));
    } else {
        assert!(
            reported.udp_send.is_none(),
            "old peer's live upload loss is unknown"
        );
        assert!(reported.udp_progress.is_none());
    }
    let recv = reported.udp_recv.unwrap();
    assert_eq!(recv.progress.packets_received, 8);
    assert_eq!(recv.progress.packets_lost, 8);
    let total_upload = fixture.facts.upload_bytes.iter().sum::<u64>();
    if version.has_aggregate_split() {
        assert_eq!(reported.bytes_sent, Some(total_upload));
        assert_eq!(
            reported.bytes_received,
            Some(INITIAL_DOWNLOAD_BYTES.iter().sum())
        );
        assert_eq!(
            reported.total_bytes,
            total_upload + INITIAL_DOWNLOAD_BYTES.iter().sum::<u64>()
        );
        assert_eq!(
            reported.throughput_mbps,
            reported.throughput_send_mbps.unwrap() + reported.throughput_recv_mbps.unwrap()
        );
    }
    assert_eq!(
        reported
            .streams
            .iter()
            .map(|stream| stream.id)
            .collect::<Vec<_>>(),
        [1, 0]
    );
    for stream in &reported.streams {
        let id = usize::from(stream.id);
        assert_eq!(stream.bytes_received, Some(INITIAL_DOWNLOAD_BYTES[id]));
        if matches!(version, PeerVersion::Directional) {
            assert_eq!(stream.bytes_sent, Some(fixture.facts.upload_bytes[id]));
            assert_eq!(
                stream.bytes,
                fixture.facts.upload_bytes[id] + INITIAL_DOWNLOAD_BYTES[id]
            );
        } else {
            assert!(
                stream.bytes_sent.is_none(),
                "legacy rows must not invent an upload split"
            );
            assert!(stream.throughput_send_mbps.is_none());
            assert_eq!(
                stream.bytes,
                fixture.facts.upload_bytes[id] + REPORTED_DOWNLOAD_BYTES
            );
        }
    }

    fixture.commands.send(PeerCommand::Tail).await.unwrap();
    let tail = wait_progress(&mut fixture.progress, |progress| {
        progress.udp_feedback_only
            && progress
                .udp_recv
                .as_ref()
                .is_some_and(|recv| recv.progress.packets_received == 10)
    })
    .await;
    let recv = tail.udp_recv.unwrap();
    assert_eq!(recv.progress.packets_lost, 8);
    assert_jitter(recv.jitter_ms);
    assert!(
        tail.udp_send.is_none(),
        "local updates must not overwrite upload readings"
    );
    assert!(tail.udp_progress.is_none());

    fixture.commands.send(PeerCommand::Result).await.unwrap();
    let result = timeout(WAIT, &mut fixture.client)
        .await
        .expect("client must finish")
        .unwrap()
        .unwrap();
    timeout(WAIT, &mut fixture.peer).await.unwrap().unwrap();
    let upload = result
        .udp_stats_send
        .as_ref()
        .expect("upload final stats, including legacy fallback");
    assert_eq!(
        upload.packets_received,
        fixture.facts.upload_stats.packets_received
    );
    assert_eq!(upload.lost, fixture.facts.upload_stats.lost);
    assert_eq!(upload.jitter_ms, 1.25);
    let legacy = result.udp_stats.as_ref().unwrap();
    assert_eq!(legacy.packets_received, 8);
    assert_eq!(legacy.lost, 6);
    let download = result
        .udp_stats_recv
        .as_ref()
        .expect("joined receiver must store final download stats");
    assert_eq!(download.packets_received, 10);
    assert_eq!(download.lost, 8);
    assert_eq!(download.packets_sent, 18);
    assert!((download.lost_percent - 8.0 / 18.0 * 100.0).abs() < 1e-9);
    assert!(download.jitter_ms.is_finite() && download.jitter_ms >= 0.0);
    assert!(download.jitter_max_ms.unwrap() >= download.jitter_ms);
    assert_eq!(download.packet_size, Some(96));
    assert_eq!(
        result.bytes_received,
        Some(FINAL_DOWNLOAD_BYTES.iter().sum())
    );
    if version.has_aggregate_split() {
        assert_eq!(result.bytes_sent, Some(total_upload));
        assert_eq!(
            result.bytes_total,
            total_upload + FINAL_DOWNLOAD_BYTES.iter().sum::<u64>()
        );
    } else {
        assert!(result.bytes_sent.is_none());
        assert_eq!(
            result.bytes_total,
            total_upload + 2 * REPORTED_DOWNLOAD_BYTES
        );
    }
    for stream in &result.streams {
        let id = usize::from(stream.id);
        assert_eq!(stream.bytes_received, Some(FINAL_DOWNLOAD_BYTES[id]));
        let expected_recv_mbps = FINAL_DOWNLOAD_BYTES[id] as f64 * 4.0 / 1_000_000.0;
        assert!((stream.throughput_recv_mbps.unwrap() - expected_recv_mbps).abs() < 1e-12);
        if matches!(version, PeerVersion::Directional) {
            assert_eq!(stream.bytes_sent, Some(fixture.facts.upload_bytes[id]));
            assert_eq!(
                stream.bytes,
                fixture.facts.upload_bytes[id] + FINAL_DOWNLOAD_BYTES[id]
            );
        } else {
            assert!(stream.bytes_sent.is_none());
            assert!(stream.throughput_send_mbps.is_none());
            assert_eq!(
                stream.bytes,
                fixture.facts.upload_bytes[id] + REPORTED_DOWNLOAD_BYTES
            );
        }
    }
}

#[tokio::test]
async fn udp_bidir_keeps_directional_loss_and_receiver_tail_independent() {
    exercise(PeerVersion::Directional).await;
}

#[tokio::test]
async fn udp_bidir_legacy_rows_do_not_fabricate_upload_splits() {
    exercise(PeerVersion::LegacyStreams).await;
}

#[tokio::test]
async fn udp_bidir_pre_split_peer_keeps_unknown_upload_bytes() {
    exercise(PeerVersion::LegacyWithoutSplits).await;
}

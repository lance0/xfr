//! Exercise reordered data through the actual receive loop, live feedback,
//! interval snapshots, and final receiver result. Every socket uses an
//! ephemeral port, so these tests also run alongside the network suite.

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::net::UdpSocket;
use tokio::sync::watch;
use xfr::client::UdpProgressFilter;
use xfr::protocol::UdpIntervalProgress;
use xfr::stats::StreamStats;
use xfr::udp::{UDP_FEEDBACK_SIZE, UdpFeedbackPacket, UdpPacketHeader, receive_udp};

async fn await_counts(stats: &StreamStats, received: u64, lost: u64) {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let progress = stats.udp_progress_snapshot();
            if progress.packets_received == received && progress.packets_lost == lost {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("receiver publishes the expected packet counts");
}

async fn send_packet(socket: &UdpSocket, sequence: u64) {
    let mut buffer = [0u8; 64];
    UdpPacketHeader {
        sequence,
        timestamp_us: sequence * 1000,
    }
    .encode(&mut buffer);
    socket.send(&buffer).await.unwrap();
}

async fn await_feedback(socket: &UdpSocket, received: u64, lost: u64) -> UdpIntervalProgress {
    tokio::time::timeout(Duration::from_secs(2), async {
        let mut buffer = [0u8; UDP_FEEDBACK_SIZE];
        loop {
            let len = socket.recv(&mut buffer).await.unwrap();
            let packet = UdpFeedbackPacket::decode(&buffer[..len]).unwrap();
            if packet.packets_received == received && packet.packets_lost == lost {
                return packet.to_progress();
            }
        }
    })
    .await
    .expect("feedback reflects the receiver's corrected counts")
}

#[tokio::test]
async fn reordered_packets_repair_live_and_final_loss_in_both_socket_modes() {
    for connected in [false, true] {
        let receiver = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        sender
            .connect(receiver.local_addr().unwrap())
            .await
            .unwrap();
        if connected {
            receiver
                .connect(sender.local_addr().unwrap())
                .await
                .unwrap();
        }
        let stats = Arc::new(StreamStats::new(0));
        let (cancel_tx, cancel) = watch::channel(false);
        let (_pause_tx, pause) = watch::channel(false);
        let task = tokio::spawn(receive_udp(
            receiver,
            stats.clone(),
            cancel,
            pause,
            true,
            connected.then_some([0u8; 24]),
        ));

        send_packet(&sender, 0).await;
        send_packet(&sender, 2).await;
        await_counts(&stats, 2, 1).await;
        let gap_interval = stats.record_interval();
        assert_eq!(gap_interval.lost, 1);
        let old_feedback = await_feedback(&sender, 2, 1).await;
        let filter = UdpProgressFilter::new();
        assert!(filter.apply(old_feedback).is_some());

        // Duplicating sequence 0 cannot dilute the current loss percentage.
        send_packet(&sender, 0).await;
        send_packet(&sender, 0).await;
        tokio::time::timeout(Duration::from_secs(2), async {
            while stats.bytes_received.load(Ordering::Relaxed) < 4 * 64 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let unchanged = stats.udp_progress_snapshot();
        assert_eq!(unchanged.packets_received, 2);
        assert_eq!(unchanged.packets_lost, 1);

        // The gap fills after an interval has already reported it as loss.
        send_packet(&sender, 1).await;
        await_counts(&stats, 3, 0).await;
        let recovered = stats.record_interval();
        assert_eq!(
            recovered.lost, 0,
            "an unsigned interval delta must not underflow"
        );
        assert_eq!(recovered.cumulative_packets_received, 3);
        assert_eq!(recovered.cumulative_packets_lost, 0);
        let new_feedback = await_feedback(&sender, 3, 0).await;
        assert_eq!(new_feedback.lost_percent(), Some(0.0));
        assert!(filter.apply(new_feedback).is_some());
        assert!(
            filter.apply(old_feedback).is_none(),
            "old equal-denominator feedback must not restore the gap"
        );

        cancel_tx.send(true).unwrap();
        let (result, inferred_sent) = task.await.unwrap().unwrap();
        assert_eq!(inferred_sent, 3);
        assert_eq!(result.packets_sent, 3);
        assert_eq!(result.packets_received, 3);
        assert_eq!(result.lost, 0);
        assert_eq!(result.lost_percent, 0.0);
        assert_eq!(
            result.out_of_order, 1,
            "duplicates are not reordered deliveries"
        );
        assert_eq!(
            stats.bytes_received.load(Ordering::Relaxed),
            5 * 64,
            "wire byte counts retain duplicate datagrams"
        );
    }
}

#[tokio::test]
async fn duplicates_do_not_hide_a_genuinely_missing_packet() {
    let receiver = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sender
        .connect(receiver.local_addr().unwrap())
        .await
        .unwrap();
    let stats = Arc::new(StreamStats::new(0));
    let (cancel_tx, cancel) = watch::channel(false);
    let (_pause_tx, pause) = watch::channel(false);
    let task = tokio::spawn(receive_udp(
        receiver,
        stats.clone(),
        cancel,
        pause,
        false,
        None,
    ));
    for sequence in [0, 2, 2, 0, 3] {
        send_packet(&sender, sequence).await;
    }
    await_counts(&stats, 3, 1).await;
    cancel_tx.send(true).unwrap();
    let (result, _) = task.await.unwrap().unwrap();
    assert_eq!(result.packets_received, 3);
    assert_eq!(result.packets_sent, 4);
    assert_eq!(result.lost, 1);
    assert_eq!(result.lost_percent, 25.0);
    assert_eq!(result.out_of_order, 0);
    assert_eq!(stats.bytes_received.load(Ordering::Relaxed), 5 * 64);
}

#[tokio::test]
async fn tail_gaps_repaired_after_the_end_are_not_loss_but_unfilled_ones_are() {
    let receiver = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sender
        .connect(receiver.local_addr().unwrap())
        .await
        .unwrap();
    let stats = Arc::new(StreamStats::new(0));
    let (cancel_tx, cancel) = watch::channel(false);
    let (_pause_tx, pause) = watch::channel(false);
    let task = tokio::spawn(receive_udp(
        receiver,
        stats.clone(),
        cancel,
        pause,
        false,
        None,
    ));
    for sequence in [0, 1, 3, 5] {
        send_packet(&sender, sequence).await;
    }
    await_counts(&stats, 4, 2).await;
    // The test ends with 2 and 4 outstanding. 2 was only reordered and lands
    // after the end; 4 never arrives; 6 was sent after the receiver stopped
    // and must not open a fresh gap at the tail.
    cancel_tx.send(true).unwrap();
    send_packet(&sender, 2).await;
    send_packet(&sender, 6).await;
    let (result, inferred_sent) = task.await.unwrap().unwrap();
    assert_eq!(inferred_sent, 6);
    assert_eq!(result.packets_received, 5);
    assert_eq!(result.lost, 1);
    assert_eq!(result.out_of_order, 1);
    assert_eq!(stats.bytes_received.load(Ordering::Relaxed), 5 * 64);
}

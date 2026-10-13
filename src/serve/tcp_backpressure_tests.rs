//! Exercise stalled control writes through the full TCP test lifecycle.

use super::*;

#[derive(Default)]
struct WriteGate {
    remaining: std::sync::atomic::AtomicUsize,
    wake: std::sync::Mutex<Option<std::task::Waker>>,
}

impl WriteGate {
    fn allow(&self, bytes: usize) {
        self.remaining
            .store(bytes, std::sync::atomic::Ordering::Release);
        if let Some(waker) = self.wake.lock().unwrap().take() {
            waker.wake();
        }
    }
}

struct GatedWriter<W> {
    inner: W,
    gate: Arc<WriteGate>,
}

impl<W: AsyncWrite + Unpin> AsyncWrite for GatedWriter<W> {
    fn poll_write(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        bytes: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        use std::sync::atomic::Ordering;
        if self.gate.remaining.load(Ordering::Acquire) == 0 {
            *self.gate.wake.lock().unwrap() = Some(cx.waker().clone());
            if self.gate.remaining.load(Ordering::Acquire) == 0 {
                return std::task::Poll::Pending;
            }
        }
        let length = bytes.len().min(self.gate.remaining.load(Ordering::Acquire));
        let result = std::pin::Pin::new(&mut self.inner).poll_write(cx, &bytes[..length]);
        if let std::task::Poll::Ready(Ok(written)) = result {
            self.gate.remaining.fetch_sub(written, Ordering::AcqRel);
        }
        result
    }

    fn poll_flush(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::pin::Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

struct StalledControlTest {
    reader: BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
    control_reader: crate::control_crypto::ControlReader,
    control_writer: crate::control_crypto::ControlWriter,
    events: mpsc::Receiver<ServerEvent>,
    cancel: watch::Receiver<bool>,
    pause: watch::Receiver<bool>,
    active_tests: Arc<Mutex<HashMap<String, ActiveTest>>>,
    id: String,
    run: JoinHandle<anyhow::Result<(u64, u64, f64)>>,
    gate: Arc<WriteGate>,
    data: Vec<TcpStream>,
    stats: Arc<TestStats>,
}

impl Drop for StalledControlTest {
    fn drop(&mut self) {
        self.run.abort();
    }
}

async fn stalled_control_test(protected: bool, duration: Duration) -> StalledControlTest {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let socket = tokio::net::TcpSocket::new_v4().unwrap();
    let (client, (server, peer)) = tokio::try_join!(
        socket.connect(listener.local_addr().unwrap()),
        listener.accept(),
    )
    .unwrap();
    server.set_nodelay(true).unwrap();
    let transport = |server: bool| {
        if protected {
            let (c2s, s2c) = crate::control_crypto::ControlCodec::derive_pair(
                "blocked-client",
                "blocked-server",
                "blocked-secret",
            )
            .unwrap();
            if server {
                crate::control_crypto::ProtectedControl::protected_server(c2s, s2c)
            } else {
                crate::control_crypto::ProtectedControl::protected_client(c2s, s2c)
            }
        } else {
            crate::control_crypto::ProtectedControl::plaintext()
        }
    };
    let server_transport = transport(true);
    let (control_reader, control_writer) = transport(false).split();
    let id = format!("blocked-control-{protected}");
    let gate = Arc::new(WriteGate::default());
    gate.allow(usize::MAX);
    let active_tests = Arc::new(Mutex::new(HashMap::new()));
    let (events_tx, events) = mpsc::channel(16);
    let tests_for_run = active_tests.clone();
    let id_for_run = id.clone();
    let (server_reader, server_writer) = server.into_split();
    let mut server_writer = GatedWriter {
        inner: server_writer,
        gate: gate.clone(),
    };
    let run = tokio::spawn(async move {
        run_test(
            BufReader::new(server_reader),
            &mut server_writer,
            &id_for_run,
            Protocol::Tcp,
            2,
            duration,
            Direction::Upload,
            None,
            None,
            tests_for_run,
            AddressFamily::V4Only,
            None,
            peer,
            Some(events_tx),
            &None,
            true,
            false,
            None,
            None,
            None,
            false,
            false,
            false,
            None,
            udp::UDP_PAYLOAD_SIZE,
            server_transport,
        )
        .await
    });
    let (reader, writer) = client.into_split();
    let mut reader = BufReader::new(reader);
    let mut control_reader = control_reader;
    let mut line = String::new();
    tokio::time::timeout(
        Duration::from_secs(5),
        control_reader.read_message(&mut reader, &mut line),
    )
    .await
    .expect("test acknowledgement")
    .unwrap();
    assert!(matches!(
        ControlMessage::deserialize(&line).unwrap(),
        ControlMessage::TestAck { .. }
    ));
    let (cancel, pause, stats, data_tx) = {
        let tests = active_tests.lock().await;
        let test = tests.get(&id).unwrap();
        (
            test.cancel_tx.subscribe(),
            test.pause_tx.subscribe(),
            test.stats.clone(),
            test.data_stream_tx.clone().unwrap(),
        )
    };
    let mut data = Vec::new();
    for index in 0..2 {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (client, (server, _)) = tokio::try_join!(
            TcpStream::connect(listener.local_addr().unwrap()),
            listener.accept(),
        )
        .unwrap();
        data_tx.send((server, index)).await.unwrap();
        data.push(client);
    }
    // Inject a transport stall seven bytes into the next control frame.
    // OS socket buffers alone cannot reliably force this on every platform.
    gate.allow(7);
    StalledControlTest {
        reader,
        writer,
        control_reader,
        control_writer,
        events,
        cancel,
        pause,
        active_tests,
        id,
        run,
        gate,
        data,
        stats,
    }
}

async fn deliver_bytes(test: &mut StalledControlTest, bytes: [usize; 2], total: u64) {
    for (stream, count) in test.data.iter_mut().zip(bytes) {
        stream.write_all(&vec![42; count]).await.unwrap();
    }
    tokio::time::timeout(Duration::from_secs(1), async {
        while test.stats.total_bytes() != total {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("data handlers must count the whole transfer");
}

async fn next_stalled_report(test: &mut StalledControlTest) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match test.events.recv().await {
                Some(ServerEvent::TestUpdated { .. }) => return,
                Some(_) => {}
                None => panic!("server stopped reporting"),
            }
        }
    })
    .await
    .expect("sampling must continue while the peer does not read reports");
}

#[tokio::test]
async fn blocked_control_keeps_sampling_and_accepts_pause_resume_cancel() {
    for protected in [false, true] {
        let mut test = stalled_control_test(protected, Duration::ZERO).await;
        next_stalled_report(&mut test).await;
        next_stalled_report(&mut test).await;
        for (message, paused) in [
            (
                ControlMessage::Pause {
                    id: test.id.clone(),
                },
                true,
            ),
            (
                ControlMessage::Resume {
                    id: test.id.clone(),
                },
                false,
            ),
        ] {
            test.control_writer
                .write_message(&mut test.writer, &message.serialize().unwrap())
                .await
                .unwrap();
            tokio::time::timeout(
                Duration::from_secs(1),
                test.pause.wait_for(|value| *value == paused),
            )
            .await
            .expect("pause state must update while output is blocked")
            .unwrap();
        }
        let cancel = ControlMessage::Cancel {
            id: test.id.clone(),
            reason: "regression".into(),
        };
        test.control_writer
            .write_message(&mut test.writer, &cancel.serialize().unwrap())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), test.cancel.wait_for(|value| *value))
            .await
            .expect("data must stop promptly while output is blocked")
            .unwrap();
        let error = tokio::time::timeout(Duration::from_secs(5), &mut test.run)
            .await
            .expect("a nonreading peer cannot retain its handler forever")
            .unwrap()
            .unwrap_err();
        assert!(error.to_string().contains("timed out"), "{error}");
        assert!(test.active_tests.lock().await.is_empty());
    }
}

#[tokio::test]
async fn blocked_control_stops_at_duration_and_delivers_framed_result_on_recovery() {
    for protected in [false, true] {
        let mut test = stalled_control_test(protected, Duration::from_secs(2)).await;
        deliver_bytes(&mut test, [1000, 2000], 3000).await;
        next_stalled_report(&mut test).await;
        // The tail arrives after the blocked report's snapshot. Final results
        // must use stopped data counters, not just already sampled intervals.
        deliver_bytes(&mut test, [3000, 4000], 10000).await;
        tokio::time::timeout(Duration::from_secs(2), test.cancel.wait_for(|value| *value))
            .await
            .expect("duration must stop data despite blocked reports")
            .unwrap();
        test.gate.allow(usize::MAX);
        // Resume reading after the duration expires. The partially sent
        // report must decode in full before the final summary, including
        // when a PSK frame prefix/payload was split across writes.
        let result = tokio::time::timeout(Duration::from_secs(5), async {
            let mut line = String::new();
            loop {
                test.control_reader
                    .read_message(&mut test.reader, &mut line)
                    .await
                    .unwrap();
                match ControlMessage::deserialize(&line).unwrap() {
                    ControlMessage::Interval { .. } => {}
                    ControlMessage::Result(result) => break result,
                    _ => panic!("unexpected control message"),
                }
            }
        })
        .await
        .expect("framing and final result must survive writer recovery");
        assert_eq!(result.bytes_total, 10000);
        assert_eq!(result.streams[0].bytes, 4000);
        assert_eq!(result.streams[1].bytes, 6000);
        assert!(
            (1800..=2600).contains(&result.duration_ms),
            "{}",
            result.duration_ms
        );
        tokio::time::timeout(Duration::from_secs(2), &mut test.run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(test.active_tests.lock().await.is_empty());
    }
}

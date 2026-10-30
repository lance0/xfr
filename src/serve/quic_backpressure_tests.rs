use super::*;

const RECEIVE_WINDOW: u32 = 64;

struct ControlPair {
    server_endpoint: quinn::Endpoint,
    client_endpoint: quinn::Endpoint,
    server_connection: quinn::Connection,
    client_connection: quinn::Connection,
    server_send: quinn::SendStream,
    server_recv: quinn::RecvStream,
    client_send: quinn::SendStream,
    client_recv: quinn::RecvStream,
    server_transport: crate::control_crypto::ProtectedControl,
    client_transport: crate::control_crypto::ProtectedControl,
}

async fn control_pair(protected: bool) -> ControlPair {
    use quinn::crypto::rustls::QuicClientConfig;

    let (cert, key) = quic::generate_self_signed_cert().unwrap();
    let mut roots = rustls::RootCertStore::empty();
    roots.add(cert[0].clone()).unwrap();
    let mut crypto = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    crypto.alpn_protocols = vec![b"xfr".to_vec()];
    let mut transport = quinn::TransportConfig::default();
    transport.stream_receive_window(quinn::VarInt::from_u32(RECEIVE_WINDOW));
    transport.receive_window(quinn::VarInt::from_u32(RECEIVE_WINDOW));
    let mut config =
        quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(crypto).unwrap()));
    config.transport_config(Arc::new(transport));

    let server_endpoint = quic::create_server_endpoint(
        "127.0.0.1:0".parse().unwrap(),
        AddressFamily::V4Only,
        cert,
        key,
        None,
    )
    .unwrap();
    let mut client_endpoint = quinn::Endpoint::client("127.0.0.1:0".parse().unwrap()).unwrap();
    client_endpoint.set_default_client_config(config);
    let (client_connection, server_connection) =
        tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(
                async {
                    client_endpoint
                        .connect(server_endpoint.local_addr().unwrap(), "xfr")
                        .unwrap()
                        .await
                        .unwrap()
                },
                async { server_endpoint.accept().await.unwrap().await.unwrap() },
            )
        })
        .await
        .expect("QUIC handshake should complete");

    let make_transport = |server: bool| {
        if protected {
            let (c2s, s2c) = crate::control_crypto::ControlCodec::derive_pair(
                "quic-blocked-client",
                "quic-blocked-server",
                "quic-blocked-secret",
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
    let server_transport = make_transport(true);
    let mut client_transport = make_transport(false);
    let (mut client_send, client_recv) = client_connection.open_bi().await.unwrap();
    // Quinn exposes a stream to the peer only after its first write. This
    // command has a different id, so the test's reader safely ignores it.
    client_transport
        .write_message(
            &mut client_send,
            &ControlMessage::Resume {
                id: "open-control-stream".to_string(),
            }
            .serialize()
            .unwrap(),
        )
        .await
        .unwrap();
    let (server_send, server_recv) =
        tokio::time::timeout(Duration::from_secs(3), server_connection.accept_bi())
            .await
            .expect("control stream should open")
            .unwrap();

    ControlPair {
        server_endpoint,
        client_endpoint,
        server_connection,
        client_connection,
        server_send,
        server_recv,
        client_send,
        client_recv,
        server_transport,
        client_transport,
    }
}

struct StalledTest {
    _server_endpoint: quinn::Endpoint,
    _client_endpoint: quinn::Endpoint,
    connection: quinn::Connection,
    reader: BufReader<quinn::RecvStream>,
    writer: quinn::SendStream,
    control_reader: crate::control_crypto::ControlReader,
    control_writer: crate::control_crypto::ControlWriter,
    events: mpsc::Receiver<ServerEvent>,
    cancel: watch::Receiver<bool>,
    pause: watch::Receiver<bool>,
    active_tests: Arc<Mutex<HashMap<String, ActiveTest>>>,
    id: String,
    run: JoinHandle<anyhow::Result<()>>,
}

impl Drop for StalledTest {
    fn drop(&mut self) {
        self.run.abort();
        self.connection.close(0u32.into(), b"test complete");
    }
}

async fn stalled_test(protected: bool, duration: Duration) -> StalledTest {
    let pair = control_pair(protected).await;
    let (control_reader, control_writer) = pair.client_transport.split();
    let active_tests = Arc::new(Mutex::new(HashMap::new()));
    let tests_for_run = active_tests.clone();
    let (events_tx, mut events) = mpsc::channel(16);
    let id = format!("quic-control-backpressure-{protected}");
    let id_for_run = id.clone();
    let run = tokio::spawn(async move {
        run_quic_test(
            &pair.server_connection,
            BufReader::new(pair.server_recv),
            pair.server_send,
            &id_for_run,
            1,
            duration,
            Direction::Upload,
            None,
            None,
            tests_for_run,
            pair.server_connection.remote_address(),
            Some(events_tx),
            &None,
            &std::collections::BTreeMap::new(),
            pair.server_transport,
        )
        .await
    });
    let started = tokio::time::timeout(Duration::from_secs(3), events.recv())
        .await
        .expect("server should start the test")
        .unwrap();
    assert!(matches!(started, ServerEvent::TestStarted(_)));
    let (cancel, pause) = {
        let tests = active_tests.lock().await;
        let test = tests.get(&id).unwrap();
        (test.cancel_tx.subscribe(), test.pause_tx.subscribe())
    };

    StalledTest {
        _server_endpoint: pair.server_endpoint,
        _client_endpoint: pair.client_endpoint,
        connection: pair.client_connection,
        reader: BufReader::new(pair.client_recv),
        writer: pair.client_send,
        control_reader,
        control_writer,
        events,
        cancel,
        pause,
        active_tests,
        id,
        run,
    }
}

async fn next_report(test: &mut StalledTest) {
    let event = tokio::time::timeout(Duration::from_secs(3), test.events.recv())
        .await
        .expect("sampling must continue while the peer leaves reports unread")
        .expect("server stopped sampling");
    assert!(
        matches!(event, ServerEvent::TestUpdated { ref id, .. } if id == &test.id),
        "unexpected event before the next sample: {event:?}",
    );
}

async fn command(test: &mut StalledTest, message: ControlMessage) {
    tokio::time::timeout(
        Duration::from_secs(1),
        test.control_writer
            .write_message(&mut test.writer, &message.serialize().unwrap()),
    )
    .await
    .expect("opposite QUIC direction should remain writable")
    .unwrap();
}

async fn read_result(
    test: &mut StalledTest,
    expect_cancelled: bool,
) -> crate::protocol::TestResult {
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut cancelled = false;
        let mut intervals = 0;
        let mut line = String::new();
        loop {
            test.control_reader
                .read_message(&mut test.reader, &mut line)
                .await
                .expect("partially written frames must remain decodable after recovery");
            match ControlMessage::deserialize(line.trim()).unwrap() {
                ControlMessage::Interval { id, .. } => {
                    assert_eq!(id, test.id);
                    assert!(!cancelled, "interval cannot follow terminal cancellation");
                    intervals += 1;
                }
                ControlMessage::Cancelled { id } => {
                    assert_eq!(id, test.id);
                    assert!(!cancelled, "cancellation acknowledgement must be sent once");
                    cancelled = true;
                }
                ControlMessage::Result(result) => {
                    assert_eq!(result.id, test.id);
                    assert_eq!(cancelled, expect_cancelled);
                    assert!(
                        intervals > 0,
                        "the in-flight interval must finish before Result"
                    );
                    return result;
                }
                message => panic!("unexpected control message: {message:?}"),
            }
        }
    })
    .await
    .expect("flow-control recovery must deliver the final result")
}

#[tokio::test]
async fn unread_quic_reports_do_not_block_sampling_or_control_commands() {
    for protected in [false, true] {
        let mut test = stalled_test(protected, Duration::ZERO).await;
        next_report(&mut test).await;
        next_report(&mut test).await;
        // An interval is larger than the 64-byte receive window. No reads
        // occurred above, so its remainder is flow-control blocked. Actual
        // STREAM reception confirms the test reached a partial wire write.
        assert!(test.connection.stats().frame_rx.stream > 0);

        let id = test.id.clone();
        command(&mut test, ControlMessage::Pause { id: id.clone() }).await;
        tokio::time::timeout(Duration::from_secs(1), test.pause.wait_for(|value| *value))
            .await
            .expect("blocked reports must not delay Pause")
            .unwrap();
        next_report(&mut test).await;
        command(&mut test, ControlMessage::Resume { id: id.clone() }).await;
        tokio::time::timeout(Duration::from_secs(1), test.pause.wait_for(|value| !*value))
            .await
            .expect("blocked reports must not delay Resume")
            .unwrap();
        command(
            &mut test,
            ControlMessage::Cancel {
                id,
                reason: "test cancellation".to_string(),
            },
        )
        .await;
        tokio::time::timeout(Duration::from_secs(1), test.cancel.wait_for(|value| *value))
            .await
            .expect("blocked reports must not delay data cancellation")
            .unwrap();

        let result = read_result(&mut test, true).await;
        assert_eq!(result.bytes_total, 0);
        tokio::time::timeout(Duration::from_secs(3), &mut test.run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(test.active_tests.lock().await.is_empty());
    }
}

#[tokio::test]
async fn unread_quic_reports_do_not_extend_active_duration() {
    let mut test = stalled_test(true, Duration::from_secs(2)).await;
    next_report(&mut test).await;
    tokio::time::timeout(Duration::from_secs(2), test.cancel.wait_for(|value| *value))
        .await
        .expect("active duration must end before the peer starts reading")
        .unwrap();
    assert!(test.connection.stats().frame_rx.stream > 0);

    let result = read_result(&mut test, false).await;
    assert!(
        (1_900..=2_600).contains(&result.duration_ms),
        "blocked output must not enter final active duration: {}",
        result.duration_ms,
    );
    tokio::time::timeout(Duration::from_secs(3), &mut test.run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(test.active_tests.lock().await.is_empty());
}

#[tokio::test]
async fn cancelled_quinn_async_write_keeps_protected_frame_offset() {
    let mut pair = control_pair(true).await;
    let (_, mut server_codec) = pair.server_transport.split();
    let (mut client_codec, _) = pair.client_transport.split();
    let message = "cancel-safe QUIC write ".repeat(16);
    let frame = server_codec
        .encode_message(
            &ControlMessage::Error {
                message: message.clone(),
            }
            .serialize()
            .unwrap(),
        )
        .unwrap();
    assert!(frame.len() > RECEIVE_WINDOW as usize);
    let written = tokio::io::AsyncWriteExt::write(&mut pair.server_send, &frame)
        .await
        .unwrap();
    assert_eq!(written, RECEIVE_WINDOW as usize);

    // Exercise Quinn's Tokio AsyncWrite implementation, which the reporting
    // loop polls in select!. Dropping a pending write must consume no bytes.
    for _ in 0..3 {
        assert!(
            tokio::time::timeout(
                Duration::from_millis(20),
                tokio::io::AsyncWriteExt::write(&mut pair.server_send, &frame[written..]),
            )
            .await
            .is_err(),
            "an unread peer must keep the remaining frame flow-control blocked",
        );
    }

    let mut reader = BufReader::new(pair.client_recv);
    let mut line = String::new();
    tokio::time::timeout(Duration::from_secs(3), async {
        let (write, read) = tokio::join!(
            tokio::io::AsyncWriteExt::write_all(&mut pair.server_send, &frame[written..]),
            client_codec.read_message(&mut reader, &mut line),
        );
        write.unwrap();
        read.unwrap();
    })
    .await
    .expect("resuming from the saved offset must deliver one complete protected frame");
    match ControlMessage::deserialize(&line).unwrap() {
        ControlMessage::Error { message: decoded } => assert_eq!(decoded, message),
        decoded => panic!("unexpected decoded message: {decoded:?}"),
    }
    pair.client_connection.close(0u32.into(), b"test complete");
}

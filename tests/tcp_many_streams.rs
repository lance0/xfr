//! Many-stream TCP load test, kept out of `integration.rs`: cargo runs test
//! binaries one at a time, so this loopback saturation never overlaps the
//! latency-sensitive integration tests.

use std::time::Duration;
use tokio::time::timeout;

// Outside integration.rs's 16000+ counter range, so a stray listener from
// either binary can never be mistaken for the other's.
const PORT: u16 = 17000;

// Own binary: its 64-stream loopback saturation stalls concurrent IPv6 control connections on macOS CI.
/// LAN-2321: with many unpaced TCP streams, the senders used to monopolize
/// the runtime's workers, so later streams' connect + DataHello (and the
/// server's routing of them) only ran once the test ended and those streams
/// carried zero bytes. Separate processes, like a real run, so each side
/// gets its own multi-thread runtime.
#[tokio::test]
#[cfg_attr(
    windows,
    ignore = "LAN-2328: most of 64 TCP upload streams carry no data on native Windows"
)]
async fn test_tcp_many_streams_all_carry_data() {
    const STREAMS: usize = 64;
    let port = PORT.to_string();
    let _server = tokio::process::Command::new(env!("CARGO_BIN_EXE_xfr"))
        .args(["serve", "-p", &port, "--no-mdns"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;

    for direction in [None, Some("-R")] {
        let streams = STREAMS.to_string();
        let mut args = vec!["127.0.0.1", "-p", &port, "-P", &streams, "-t", "2sec"];
        args.extend(["--no-tui", "--json"]);
        args.extend(direction);
        let output = timeout(
            Duration::from_secs(30),
            tokio::process::Command::new(env!("CARGO_BIN_EXE_xfr"))
                .args(&args)
                .kill_on_drop(true)
                .output(),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let result: xfr::protocol::TestResult = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(result.streams.len(), STREAMS);
        let empty: Vec<u8> = result
            .streams
            .iter()
            .filter(|s| s.bytes == 0)
            .map(|s| s.id)
            .collect();
        assert!(
            empty.is_empty(),
            "{direction:?}: streams {empty:?} carried no data"
        );
        assert_eq!(result.zero_byte_streams, None);
    }
}

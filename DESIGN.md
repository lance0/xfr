# xfr — Modern Network Bandwidth Testing

A fast, beautiful iperf replacement with TUI. Built in Rust.

---

## Why This Exists

iperf3 is the de facto standard but has real problems:

| Problem                     | Impact                                     |
| --------------------------- | ------------------------------------------ |
| Single-threaded server      | Can't handle multiple simultaneous clients |
| Ugly, noisy output          | Hard to parse, hard to read                |
| No visualization            | Just walls of text                         |
| UDP stats are confusing     | Jitter/loss reported inconsistently        |
| No historical comparison    | Can't diff runs easily                     |
| Server setup friction       | No easy "just listen" mode                 |
| JSON output is verbose      | Parsing requires ceremony                  |
| No modern TUI               | 2024 and we're still reading scrollback    |

**xfr** fixes all of this.

---

## Core Differentiators

| Feature          | iperf3              | xfr                                       |
| ---------------- | ------------------- | ----------------------------------------- |
| Live TUI         | No                  | Yes — sparklines, per-stream bars         |
| Multi-client     | No (1 at a time)    | Yes — concurrent clients                  |
| Output           | Ugly text           | Clean text + JSON + Prometheus            |
| Compare runs     | No                  | `xfr diff baseline.json current.json`     |
| Visualization    | None                | Real-time graphs                          |
| Install          | Package manager     | `cargo install xfr` / single static binary |
| Code             | C, complex          | Rust, maintainable                        |
| Discovery        | Manual              | `xfr discover` finds peers on LAN         |
| Zero-config      | No                  | `xfr serve` with sensible defaults        |

---

## Architecture

```
┌────────────────────────────────────────────────────────────────────┐
│                              CLI                                    │
│                                                                     │
│  xfr serve                    # Start server (default port 5201)   │
│  xfr 192.168.1.1              # Basic TCP test, 10s                │
│  xfr 192.168.1.1 -u -b 1G     # UDP at 1 Gbps                      │
│  xfr 192.168.1.1 -P 4 -R      # 4 streams, reverse (download)      │
│  xfr diff a.json b.json       # Compare two runs                   │
│  xfr discover                 # Find xfr servers on LAN            │
└────────────────────────────────────────────────────────────────────┘
                                 │
                                 ▼
┌────────────────────────────────────────────────────────────────────┐
│                         Mode Router                                 │
├──────────────────┬─────────────────────┬───────────────────────────┤
│      serve       │       client        │          diff             │
│                  │                     │                           │
│ • Listen         │ • Connect           │ • Load JSON results       │
│ • Multi-client   │ • Run test          │ • Compute deltas          │
│ • Report back    │ • Display TUI       │ • Report regressions      │
└──────────────────┴─────────────────────┴───────────────────────────┘
                                 │
                                 ▼
┌────────────────────────────────────────────────────────────────────┐
│                        Protocol Layer                               │
├─────────────────────┬──────────────────────┬───────────────────────┤
│         TCP         │         UDP          │     QUIC (future)     │
│                     │                      │                       │
│ • Bulk send         │ • Paced send         │ • quinn crate         │
│ • Measure Gbps      │ • Jitter/loss calc   │ • Modern transport    │
│ • Retransmit stats  │ • Sequence tracking  │ • 0-RTT, multiplexing │
└─────────────────────┴──────────────────────┴───────────────────────┘
                                 │
                                 ▼
┌────────────────────────────────────────────────────────────────────┐
│                       Metrics Collector                             │
│                                                                     │
│ • Bytes transferred (per-stream, aggregate)                        │
│ • Time elapsed (wall clock, CPU time)                              │
│ • Throughput (Mbps/Gbps, rolling average)                          │
│ • Jitter (UDP only, RFC 3550 style)                                │
│ • Packet loss % (UDP only)                                         │
│ • Retransmits (TCP, via getsockopt TCP_INFO)                       │
│ • RTT samples (TCP_INFO)                                           │
│ • Congestion window (TCP_INFO)                                     │
└────────────────────────────────────────────────────────────────────┘
                                 │
                                 ▼
┌────────────────────────────────────────────────────────────────────┐
│                         Output Layer                                │
├────────────┬────────────┬─────────────┬────────────────────────────┤
│    TUI     │   Plain    │    JSON     │       Prometheus           │
│  ratatui   │   Simple   │  Parseable  │       /metrics             │
│            │   text     │   for CI    │       for Grafana          │
│ Sparklines │  Numbers   │  Structured │       scrape endpoint      │
│ Per-stream │  Summary   │  Full data  │       push gateway         │
└────────────┴────────────┴─────────────┴────────────────────────────┘
```

---

## Control Protocol

JSON-over-TCP control channel. Data streams are separate connections.

```
Port 5201:  Control channel (JSON messages, newline-delimited)
Port 5202+: Data streams (raw bytes for TCP, datagrams for UDP)
```

### Version Negotiation

Both sides advertise their version in the hello message. The connection proceeds if:
- Major versions match (e.g., `1.x` and `1.y` are compatible)
- Minor version differences are tolerated (newer side may disable unsupported features)

If versions are incompatible, the server sends an error and closes the connection.

### Handshake

```json
// Client → Server
{"type":"hello","version":"1.0","client":"xfr/0.1.0"}

// Server → Client
{"type":"hello","version":"1.0","server":"xfr/0.1.0","capabilities":["tcp","udp","multistream"]}
```

### Test Request

```json
// Client → Server
{
  "type": "test_start",
  "id": "abc123",
  "protocol": "tcp",
  "streams": 4,
  "duration_secs": 10,
  "direction": "upload",
  "bitrate": null
}

// Server → Client
{
  "type": "test_ack",
  "id": "abc123",
  "data_ports": [5202, 5203, 5204, 5205]
}
```

**Direction values:**
- `"upload"` — client sends to server (default)
- `"download"` — server sends to client (reverse mode, `-R`)
- `"bidir"` — simultaneous both directions

**Bitrate:**
- `null` — unlimited (TCP default)
- Integer — target bits per second (required for UDP)

### During Test

Server sends interval updates every second:

```json
{
  "type": "interval",
  "id": "abc123",
  "elapsed_ms": 1000,
  "streams": [
    {"id": 0, "bytes": 31250000, "retransmits": 2},
    {"id": 1, "bytes": 31250000, "retransmits": 0},
    {"id": 2, "bytes": 31250000, "retransmits": 1},
    {"id": 3, "bytes": 31250000, "retransmits": 0}
  ],
  "aggregate": {
    "bytes": 125000000,
    "throughput_mbps": 1000.0,
    "retransmits": 3
  }
}
```

### Test Complete

```json
{
  "type": "result",
  "id": "abc123",
  "bytes_total": 1250000000,
  "duration_ms": 10000,
  "throughput_mbps": 1000.0,
  "streams": [
    {"id": 0, "bytes": 312500000, "throughput_mbps": 250.0, "retransmits": 12},
    {"id": 1, "bytes": 312500000, "throughput_mbps": 250.0, "retransmits": 8},
    {"id": 2, "bytes": 312500000, "throughput_mbps": 250.0, "retransmits": 14},
    {"id": 3, "bytes": 312500000, "throughput_mbps": 250.0, "retransmits": 8}
  ],
  "tcp_info": {
    "retransmits": 42,
    "rtt_us": 1234,
    "rtt_var_us": 100,
    "cwnd": 65535
  }
}
```

### Cancel Test

Either side can cancel a running test:

```json
{"type":"cancel","id":"abc123","reason":"user requested"}
```

The receiving side should gracefully stop data transfer and acknowledge:

```json
{"type":"cancelled","id":"abc123"}
```

### Error

```json
{"type":"error","message":"Port already in use"}
```

---

## Data Structures

```rust
// src/protocol.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlMessage {
    Hello {
        version: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        client: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        server: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        capabilities: Option<Vec<String>>,
    },
    TestStart {
        id: String,
        protocol: Protocol,
        streams: u8,
        duration_secs: u32,
        direction: Direction,
        bitrate: Option<u64>,
    },
    TestAck {
        id: String,
        data_ports: Vec<u16>,
    },
    Interval {
        id: String,
        elapsed_ms: u64,
        streams: Vec<StreamInterval>,
        aggregate: AggregateInterval,
    },
    Result(TestResult),
    Cancel {
        id: String,
        reason: String,
    },
    Cancelled {
        id: String,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    Upload,
    Download,
    Bidir,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamInterval {
    pub id: u8,
    pub bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retransmits: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jitter_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateInterval {
    pub bytes: u64,
    pub throughput_mbps: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retransmits: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    pub id: String,
    pub bytes_total: u64,
    pub duration_ms: u64,
    pub throughput_mbps: f64,
    pub streams: Vec<StreamResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tcp_info: Option<TcpInfoSnapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub udp_stats: Option<UdpStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamResult {
    pub id: u8,
    pub bytes: u64,
    pub throughput_mbps: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retransmits: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jitter_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpInfoSnapshot {
    pub retransmits: u64,
    pub rtt_us: u32,
    pub rtt_var_us: u32,
    pub cwnd: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdpStats {
    pub packets_sent: u64,
    pub packets_received: u64,
    pub lost: u64,
    pub lost_percent: f64,
    pub jitter_ms: f64,
    pub out_of_order: u64,
}
```

```rust
// src/stats.rs
use std::sync::atomic::AtomicU64;
use std::time::Instant;

pub struct StreamStats {
    pub stream_id: u8,
    pub bytes_sent: AtomicU64,
    pub bytes_received: AtomicU64,
    pub start_time: Instant,
    pub intervals: parking_lot::Mutex<Vec<IntervalStats>>,
}

pub struct IntervalStats {
    pub timestamp: Instant,
    pub bytes: u64,
    pub throughput_mbps: f64,
    pub retransmits: u64,
    pub jitter_ms: f64,
    pub lost: u64,
}
```

---

## Project Structure

```
xfr/
├── Cargo.toml
├── README.md
├── DESIGN.md
├── src/
│   ├── main.rs           # CLI entry (clap)
│   ├── lib.rs            # Library re-exports
│   ├── serve.rs          # Server mode
│   ├── client.rs         # Client mode
│   ├── diff.rs           # Compare mode
│   ├── discover.rs       # LAN discovery (mDNS/broadcast)
│   ├── protocol.rs       # Control messages, (de)serialization
│   ├── tcp.rs            # TCP data transfer
│   ├── udp.rs            # UDP data transfer + pacing
│   ├── stats.rs          # Metrics collection
│   ├── tcp_info.rs       # Linux TCP_INFO parsing
│   ├── tui/
│   │   ├── mod.rs
│   │   ├── app.rs        # TUI state machine
│   │   ├── ui.rs         # ratatui rendering
│   │   └── widgets.rs    # Custom sparklines, gauges
│   └── output/
│       ├── mod.rs
│       ├── plain.rs      # Simple text output
│       ├── json.rs       # JSON output
│       └── prometheus.rs # /metrics endpoint
├── tests/
│   ├── integration.rs    # End-to-end tests
│   └── protocol.rs       # Protocol parsing tests
└── benches/
    └── throughput.rs     # Performance regression tests
```

---

## CLI Design

```bash
# Server mode
xfr serve                        # Listen on 5201
xfr serve -p 9000                # Custom port
xfr serve --one-off              # Exit after one test
xfr serve --prometheus 9090      # Expose metrics endpoint

# Client mode
xfr 192.168.1.1                  # TCP test, 10s, single stream
xfr 192.168.1.1 -t 30s           # 30 second test
xfr 192.168.1.1 -u               # UDP mode
xfr 192.168.1.1 -u -b 1G         # UDP at 1 Gbps
xfr 192.168.1.1 -P 4             # 4 parallel streams
xfr 192.168.1.1 -R               # Reverse (download test)
xfr 192.168.1.1 --bidir          # Bidirectional

# Output
xfr 192.168.1.1 --json           # JSON to stdout
xfr 192.168.1.1 -o results.json  # Save to file
xfr 192.168.1.1 --no-tui         # Plain text, no TUI

# Advanced TCP tuning
xfr 192.168.1.1 --tcp-nodelay    # Disable Nagle
xfr 192.168.1.1 --window 512K    # Set TCP window
xfr 192.168.1.1 --mss 1400       # Set MSS

# Diff mode
xfr diff baseline.json current.json
xfr diff baseline.json current.json --threshold 5%

# Discovery
xfr discover                     # Find xfr servers on LAN
xfr discover --timeout 5s
```

### Flags Summary

| Flag            | Short | Default    | Description                                          |
| --------------- | ----- | ---------- | ---------------------------------------------------- |
| `--port`        | `-p`  | 5201       | Server/client port                                   |
| `--time`        | `-t`  | 10s        | Test duration                                        |
| `--udp`         | `-u`  | false      | UDP mode                                             |
| `--bitrate`     | `-b`  | unlimited  | Target bitrate (TCP: pacing, UDP: required)          |
| `--parallel`    | `-P`  | 1          | Number of parallel streams                           |
| `--reverse`     | `-R`  | false      | Reverse direction (server → client)                  |
| `--bidir`       |       | false      | Bidirectional test                                   |
| `--json`        |       | false      | JSON output                                          |
| `--output`      | `-o`  | stdout     | Output file                                          |
| `--no-tui`      |       | false      | Disable TUI                                          |
| `--tcp-nodelay` |       | false      | Disable Nagle algorithm                              |
| `--window`      |       | OS default | TCP window size                                      |
| `--mss`         |       | OS default | Maximum segment size                                 |
| `--one-off`     |       | false      | Server exits after one test                          |
| `--prometheus`  |       | disabled   | Prometheus metrics port                              |
| `--threshold`   |       | 0%         | Regression threshold for diff                        |

---

## TUI Layout

### TCP Mode

```
┌─ xfr ──────────────────────────────────────────────────── 192.168.1.1:5201 ─┐
│                                                                             │
│  Protocol: TCP    Streams: 4    Direction: Upload    Elapsed: 8s / 10s     │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  Throughput                                                                 │
│  ▁▂▃▅▆▇█▇█▇▆▇█▇▆▇█▇▆▇█▇▆▇█▇▆▇█▇▆▇█▇▆▇█▇█▇▆▇█▇▆▇   943 Mbps ━━━━━━━━━━━━  │
│                                                                             │
│  Streams                                                                    │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │ [0] ████████████████████████████████████░░░░░  236 Mbps  rtx: 2     │   │
│  │ [1] █████████████████████████████████████░░░░  237 Mbps  rtx: 0     │   │
│  │ [2] ████████████████████████████████████░░░░░  234 Mbps  rtx: 1     │   │
│  │ [3] █████████████████████████████████████░░░░  236 Mbps  rtx: 0     │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                             │
│  Transfer: 1.17 GB    Retransmits: 3    RTT: 1.2ms    Cwnd: 64KB           │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│  [q] Quit   [p] Pause   [r] Restart   [j] JSON   [?] Help                  │
└─────────────────────────────────────────────────────────────────────────────┘
```

### UDP Mode

```
┌─ xfr ──────────────────────────────────────────────────── 192.168.1.1:5201 ─┐
│                                                                             │
│  Protocol: UDP    Bitrate: 1 Gbps    Direction: Upload    Elapsed: 8s/10s  │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                             │
│  Throughput                                                                 │
│  ▁▂▃▅▆▇█▇█▇▆▇█▇▆▇█▇▆▇█▇▆▇█▇▆▇█▇▆▇█▇▆▇█▇█▇▆▇█▇▆▇   987 Mbps ━━━━━━━━━━━━  │
│                                                                             │
│  Packet Loss                                           Jitter               │
│  ▁▁▁▂▁▁▁▃▁▁▁▂▁▁▁▁▂▁▁▁▁▁▂▁▁▁▃▁▁▁▁▁▂▁▁▁▁▂▁▁▁▁▁▁  0.12%   0.34 ms           │
│                                                                             │
│  Packets: 892,857 sent / 891,785 received    Lost: 1,072    OOO: 23        │
│                                                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│  [q] Quit   [p] Pause   [r] Restart   [j] JSON   [?] Help                  │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## UDP Pacing

UDP requires rate limiting. Naive "blast as fast as possible" saturates buffers and gives meaningless results.

```rust
// src/udp.rs
use tokio::time::{interval, Duration, Instant};

const UDP_PAYLOAD_SIZE: usize = 1400;  // Leave room for IP + UDP headers

/// UDP packet header (16 bytes)
/// [seq: u64][timestamp_us: u64][payload...]
///
/// Timestamps are relative to test start (not wall clock) to avoid
/// clock synchronization issues between client and server.
struct UdpPacket {
    sequence: u64,
    timestamp_us: u64,
    // remaining bytes are padding
}

pub async fn send_udp_paced(
    socket: &UdpSocket,
    target_bitrate: u64,
    duration: Duration,
) -> UdpSendStats {
    let packet_size = UDP_PAYLOAD_SIZE;
    let bits_per_packet = (packet_size * 8) as u64;
    let packets_per_sec = target_bitrate / bits_per_packet;
    let pacing_interval = Duration::from_secs_f64(1.0 / packets_per_sec as f64);

    let mut sequence: u64 = 0;
    let mut ticker = interval(pacing_interval);
    let start = Instant::now();

    let mut packet = vec![0u8; packet_size];

    while start.elapsed() < duration {
        ticker.tick().await;

        // Build packet with relative timestamp
        let now_us = start.elapsed().as_micros() as u64;
        packet[0..8].copy_from_slice(&sequence.to_be_bytes());
        packet[8..16].copy_from_slice(&now_us.to_be_bytes());

        if socket.send(&packet).await.is_err() {
            // Handle send errors (buffer full, etc.)
        }

        sequence += 1;
    }

    UdpSendStats {
        packets_sent: sequence,
        bytes_sent: sequence * packet_size as u64,
    }
}
```

### Jitter Calculation

Jitter is calculated per RFC 3550 using the interarrival time difference:

```
D(i) = (R(i) - R(i-1)) - (S(i) - S(i-1))
J(i) = J(i-1) + (|D(i)| - J(i-1)) / 16
```

Where `S` is the sender timestamp and `R` is the receiver timestamp. Since both are relative to their respective test start times, clock skew doesn't affect the calculation.

---

## TCP Info (Linux)

Linux exposes rich TCP stats via `getsockopt(TCP_INFO)`:

```rust
// src/tcp_info.rs
use std::mem;

#[repr(C)]
#[derive(Debug, Default)]
pub struct TcpInfo {
    pub tcpi_state: u8,
    pub tcpi_ca_state: u8,
    pub tcpi_retransmits: u8,
    pub tcpi_probes: u8,
    pub tcpi_backoff: u8,
    pub tcpi_options: u8,
    // ... more fields
    pub tcpi_rtt: u32,          // RTT in microseconds
    pub tcpi_rttvar: u32,       // RTT variance
    pub tcpi_snd_cwnd: u32,     // Congestion window
    pub tcpi_total_retrans: u32,
    // ... etc
}

pub fn get_tcp_info(fd: i32) -> std::io::Result<TcpInfo> {
    let mut info: TcpInfo = Default::default();
    let mut len = mem::size_of::<TcpInfo>() as libc::socklen_t;

    let ret = unsafe {
        libc::getsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_INFO,
            &mut info as *mut _ as *mut libc::c_void,
            &mut len,
        )
    };

    if ret == 0 {
        Ok(info)
    } else {
        Err(std::io::Error::last_os_error())
    }
}
```

### Platform Notes

| Platform | TCP_INFO Support                                    |
| -------- | --------------------------------------------------- |
| Linux    | Full support via `getsockopt`                       |
| macOS    | Partial — different struct, fewer fields            |
| Windows  | Not available — basic stats only (bytes, duration)  |
| FreeBSD  | Similar to Linux                                    |

For macOS, use `TCP_CONNECTION_INFO` with a different struct layout.

---

## Buffer Sizes for High-Speed Testing

For 10 Gbps+ testing, default OS buffer sizes are often insufficient:

```rust
// Recommended buffer sizes for high-speed testing
const HIGH_SPEED_BUFFER: usize = 4 * 1024 * 1024;  // 4 MB

fn configure_socket_buffers(socket: &TcpStream) -> io::Result<()> {
    let fd = socket.as_raw_fd();

    unsafe {
        let size = HIGH_SPEED_BUFFER as libc::c_int;
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_SNDBUF,
            &size as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &size as *const _ as *const libc::c_void,
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        );
    }

    Ok(())
}
```

The `--window` flag allows users to override this for specific scenarios.

---

## Connection Failure Handling

### Data Stream Disconnection

If a data stream disconnects mid-test:

1. Mark that stream as failed in the stats
2. Continue other streams if multi-stream test
3. Report partial results with error flag
4. Server sends interval updates with stream marked as disconnected

```json
{
  "type": "interval",
  "id": "abc123",
  "streams": [
    {"id": 0, "bytes": 31250000, "retransmits": 2},
    {"id": 1, "bytes": 0, "error": "connection reset"},
    {"id": 2, "bytes": 31250000, "retransmits": 1}
  ]
}
```

### Control Channel Disconnection

If the control channel drops:

1. Both sides stop data transfer immediately
2. Server cleans up resources for that test
3. Client reports error to user with last known stats

---

## Diff Mode

Compare two test runs with statistical analysis:

```bash
$ xfr diff baseline.json current.json

┌─ xfr diff ────────────────────────────────────────────────────────────────────┐
│                                                                               │
│  Baseline: baseline.json (2024-01-15 10:30:00)                                │
│  Current:  current.json  (2024-01-15 11:45:00)                                │
│                                                                               │
├───────────────────────────────────────────────────────────────────────────────┤
│                                                                               │
│  Throughput:  943.2 Mbps → 912.5 Mbps  (-3.3%)  ⚠                             │
│  Retransmits:      42 →      67        (+59.5%) ✗                             │
│  RTT:          1.2 ms →  1.4 ms        (+16.7%) ⚠                             │
│                                                                               │
│  Verdict: REGRESSION DETECTED                                                 │
│                                                                               │
└───────────────────────────────────────────────────────────────────────────────┘
```

**Exit codes for CI:**
- `0` — no regression
- `1` — regression detected (throughput dropped > threshold)
- `2` — error (file not found, invalid JSON, etc.)

---

## LAN Discovery

Find xfr servers without knowing IPs:

```bash
$ xfr discover

Searching for xfr servers...

Found 2 servers:
  192.168.1.10:5201  (macbook.local)     xfr/0.1.0
  192.168.1.50:5201  (server.local)      xfr/0.1.0

$ xfr 192.168.1.10
```

**Implementation:** mDNS service advertisement (`_xfr._tcp.local`) using the `mdns-sd` crate. Servers register on startup, clients browse for services with a configurable timeout.

---

## Security Considerations

xfr v0.1 assumes a trusted network environment. There is no authentication or encryption on the control channel or data streams.

**Recommendations:**
- Only run `xfr serve` on trusted networks
- Use firewall rules to restrict access to port 5201
- For untrusted networks, tunnel over SSH or VPN

**Future (v0.2+):**
- Optional TLS for control channel
- Pre-shared key authentication
- Rate limiting to prevent abuse

---

## Cargo.toml

```toml
[package]
name = "xfr"
version = "0.1.0"
edition = "2021"
description = "Modern network bandwidth testing with TUI"
license = "MIT"
repository = "https://github.com/lance0/xfr"
keywords = ["network", "bandwidth", "iperf", "benchmark", "tui"]
categories = ["command-line-utilities", "network-programming"]

[dependencies]
# Async runtime
tokio = { version = "1", features = ["full", "net", "time", "sync", "rt-multi-thread", "macros"] }

# TUI
ratatui = "0.29"
crossterm = "0.28"

# CLI
clap = { version = "4", features = ["derive", "env"] }

# Serialization
serde = { version = "1", features = ["derive"] }
serde_json = "1"

# Metrics
prometheus = { version = "0.13", optional = true }
hyper = { version = "1", features = ["server", "http1"], optional = true }

# Discovery
mdns-sd = { version = "0.11", optional = true }

# Misc
anyhow = "1"
thiserror = "1"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
parking_lot = "0.12"
humantime = "2"
bytesize = "1"

# Platform-specific
[target.'cfg(target_os = "linux")'.dependencies]
libc = "0.2"

[target.'cfg(target_os = "macos")'.dependencies]
libc = "0.2"

[features]
default = ["discovery"]
prometheus = ["dep:prometheus", "dep:hyper"]
discovery = ["dep:mdns-sd"]

[dev-dependencies]
tokio-test = "0.4"
criterion = { version = "0.5", features = ["async_tokio"] }

[[bench]]
name = "throughput"
harness = false

[profile.release]
lto = true
codegen-units = 1
strip = true
```

---

## Implementation Roadmap

### Phase 1: Core
- [ ] Project scaffold + clap CLI
- [ ] Control protocol (JSON messages)
- [ ] TCP single-stream transfer
- [ ] Basic server/client modes
- [ ] Plain text output

### Phase 2: Usable
- [ ] TUI with live sparklines
- [ ] Multi-stream support
- [ ] JSON output
- [ ] Reverse mode (`-R`)
- [ ] TCP_INFO stats (Linux)

### Phase 3: Feature Parity
- [ ] UDP mode with pacing
- [ ] Jitter/loss calculation
- [ ] Bidirectional mode
- [ ] Diff command
- [ ] macOS support

### Phase 4: Polish
- [ ] Prometheus metrics
- [ ] LAN discovery
- [ ] Config file support
- [ ] man page
- [ ] CI, releases, crates.io
- [ ] QUIC support (quinn)

---

## Testing Strategy

### Unit Tests
- Protocol serialization round-trips
- UDP packet construction
- TCP_INFO parsing
- Stats aggregation
- Jitter calculation

### Integration Tests
- Server/client handshake
- Single stream transfer
- Multi-stream transfer
- Reverse mode
- Bidirectional mode
- JSON output format
- Graceful cancellation

### Performance Tests
- Throughput doesn't regress
- Low overhead at 10 Gbps
- TUI doesn't drop frames
- Memory usage is bounded

### Manual Testing
- Real hardware (10G NICs)
- Cross-platform (Linux/macOS)
- Long duration stability (1+ hour)
- High packet loss scenarios (UDP)

---

## Open Questions

1. **Windows support?** — TCP_INFO doesn't exist. Fallback to basic stats only. Consider for v0.2.

2. **QUIC priority?** — Interesting but iperf3 doesn't have it either. Defer to v0.2.

3. **IPv6?** — Support from the start, but test with v4 first.

4. **Authentication?** — Out of scope for v0.1. Document security assumptions.

5. **Config file?** — Nice to have for server deployments. Consider TOML in `~/.config/xfr/config.toml`.

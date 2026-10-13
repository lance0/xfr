//! Control protocol for xfr client-server communication
//!
//! The control channel uses newline-delimited JSON messages over TCP/QUIC:
//!
//! ```text
//! {"type":"Hello","version":"1.1",...}\n
//! {"type":"ServerHello",...}\n
//! {"type":"TestStart",...}\n
//! ```
//!
//! Each message is a single JSON object followed by a newline character.
//!
//! # Protocol Versioning
//!
//! Protocol version follows semver conventions:
//! - Major version changes break compatibility
//! - Minor version changes are backwards compatible
//!
//! Use [`versions_compatible`] to check compatibility.
//!
//! # Message Flow
//!
//! ```text
//! Client                          Server
//!   │                               │
//!   │──────── Hello ───────────────>│  (control connection)
//!   │<─────── Hello (+ auth?) ──────│
//!   │──────── AuthResponse? ───────>│
//!   │<─────── AuthSuccess? ─────────│
//!   │──────── TestStart ───────────>│
//!   │<─────── TestAck ──────────────│
//!   │                               │
//!   │──────── DataHello ───────────>│  (data connection 1, same port)
//!   │──────── DataHello ───────────>│  (data connection 2, same port)
//!   │      [Data Transfer]          │
//!   │                               │
//!   │<─────── Interval ─────────────│ (periodic, on control)
//!   │<─────── Result ───────────────│
//!   │                               │
//! ```
//!
//! For UDP tests, when both peers advertise the `udp_feedback_v1`
//! capability, an out-of-band 36-byte receiver-progress packet flows
//! server→client at 2 Hz on the data UDP socket alongside the periodic
//! `Interval` messages on the control channel. The wire format and
//! framing live in [`crate::udp::UdpFeedbackPacket`]; the capability
//! check helper is [`capability_advertised`]. This sidesteps TCP
//! control for live UDP loss visibility under upload-mode saturation
//! (issue #70).

use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: &str = "1.1";
pub const DEFAULT_PORT: u16 = 5201;

/// Check if two protocol versions are compatible.
///
/// Compatibility rules:
/// - Major versions must match exactly
/// - Minor version differences are allowed (backwards compatible)
/// - Malformed versions are incompatible
pub fn versions_compatible(version_a: &str, version_b: &str) -> bool {
    match (
        parse_protocol_major(version_a),
        parse_protocol_major(version_b),
    ) {
        (Some(major_a), Some(major_b)) => major_a == major_b,
        _ => false,
    }
}

fn parse_protocol_major(version: &str) -> Option<u32> {
    let mut parts = version.split('.');
    let first = parts.next()?;
    if first.is_empty() {
        return None;
    }
    let major = first.parse().ok()?;

    for part in parts {
        if part.is_empty() {
            return None;
        }
        part.parse::<u32>().ok()?;
    }

    Some(major)
}

/// Authentication challenge sent by server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthChallenge {
    pub method: String,
    pub nonce: String,
}

/// Authentication response from client
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthResponse {
    pub response: String,
}

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
        #[serde(skip_serializing_if = "Option::is_none")]
        auth: Option<AuthChallenge>,
        /// Client nonce for protected control channel (PSK sessions).
        /// Present when the client supports `protected_control_v1`; bound
        /// into the transcript MAC so a MITM cannot strip it.
        #[serde(skip_serializing_if = "Option::is_none")]
        client_nonce: Option<String>,
    },
    AuthResponse {
        response: String,
    },
    AuthSuccess {
        /// Server proof-of-PSK binding both Hello transcripts. Present when
        /// the session is using `protected_control_v1`. The client verifies
        /// this before trusting the session.
        #[serde(skip_serializing_if = "Option::is_none")]
        server_proof: Option<String>,
    },
    TestStart {
        id: String,
        protocol: Protocol,
        streams: u8,
        duration_secs: u32,
        direction: Direction,
        #[serde(skip_serializing_if = "Option::is_none")]
        bitrate: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        congestion: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        mptcp: bool,
        /// DSCP/TOS byte for QoS marking on server-side sockets (download/bidir)
        #[serde(default, skip_serializing_if = "Option::is_none")]
        dscp: Option<u8>,
        /// SO_SNDBUF/SO_RCVBUF size requested by the client (iperf-style `-w`).
        /// When absent, the server leaves kernel autotuning enabled. `u64` so
        /// that large values like `-w 4G` round-trip without silent truncation.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window_size: Option<u64>,
        /// Request zero-copy (sendfile) sends on the server side for
        /// download/bidir TCP (`--zerocopy`, issue #33). Wire-additive:
        /// absent means false, and older servers ignore the field — the
        /// client warns via the `zerocopy_v1` capability when that happens.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        zerocopy: bool,
        /// Run a path-MTU probe instead of a throughput test (issue #64,
        /// `--probe-mtu`). The server answers variable-size `XFRP` probe
        /// packets on the data socket with an ack + same-size echo
        /// instead of running bulk handlers. Wire-additive: absent means
        /// false; the client refuses to start against a server that
        /// doesn't advertise `mtu_probe_v1` (an old server would treat
        /// probes as junk data and never reply).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        mtu_probe: bool,
        /// Propagate the client's `--tcp-nodelay` to the server's TCP data
        /// sockets — in download/bidir the server is the latency-relevant
        /// bulk sender and previously never heard about the flag. The
        /// server ORs this into its own nodelay default rather than
        /// replacing it, so the field can only ever add `TCP_NODELAY`,
        /// never disable it. Wire-additive: absent means false, and older
        /// servers ignore the field and keep their default (same
        /// acceptable-degradation call as `window_size`, no capability).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        tcp_nodelay: bool,
        /// Total payload bytes to transfer before the test ends (`-n`),
        /// instead of running for a fixed time. Applies to whichever side
        /// sends: the server enforces it on its own send loops in
        /// download/bidir, and learns the client half is finished from the
        /// `Finish` message. `duration_secs` remains an upper bound when
        /// the client passed an explicit `-t`. Wire-additive, but gated on
        /// the `byte_budget_v1` capability — an older server would ignore
        /// the field and silently run a plain timed test instead.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        byte_budget: Option<u64>,
        /// Complete UDP payload size, including the xfr header (`-l`).
        /// Absent preserves the 1400-byte default. Explicit sizes require
        /// `udp_payload_size_v1` so older servers cannot silently ignore them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        udp_payload_size: Option<u16>,
    },
    TestAck {
        id: String,
        data_ports: Vec<u16>,
        /// Per-test routing token for single-port UDP mode (issue #63),
        /// hex-encoded 16 random bytes. Present (with empty `data_ports`)
        /// when the server selected single-port UDP; the client echoes it
        /// inside each per-stream UDP hello datagram so the server can
        /// route hellos to the right test even across NAT or multiple
        /// concurrent clients. Wire-additive: absent for TCP/QUIC and for
        /// legacy multi-port UDP.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        udp_token: Option<String>,
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
    /// The client's send loops have delivered their whole `-n` byte budget.
    /// Unlike `Cancel` this is a normal end of test: the server stops the
    /// clock and replies with the usual `Result`. Only sent for byte-budget
    /// tests where the client sends (upload and bidir).
    Finish {
        id: String,
    },
    Pause {
        id: String,
    },
    Resume {
        id: String,
    },
    Error {
        message: String,
    },
    /// Data channel handshake for single-port TCP mode
    /// Sent by client on each data connection to identify stream
    DataHello {
        test_id: String,
        stream_index: u16,
    },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Tcp,
    Udp,
    Quic,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Protocol::Tcp => write!(f, "TCP"),
            Protocol::Udp => write!(f, "UDP"),
            Protocol::Quic => write!(f, "QUIC"),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    #[default]
    Upload,
    Download,
    Bidir,
}

impl std::fmt::Display for Direction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Direction::Upload => write!(f, "Upload"),
            Direction::Download => write!(f, "Download"),
            Direction::Bidir => write!(f, "Bidirectional"),
        }
    }
}

/// Cumulative UDP packet counts, sent on every periodic Interval message so
/// the client can both derive a live loss percentage and detect that the
/// server is reporting fresh data (vs. paired with an old server that omits
/// the field). Unique received packets and the expected denominator
/// (`received + lost`) grow monotonically; lost can fall as late packets
/// fill previously observed gaps.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct UdpIntervalProgress {
    pub packets_received: u64,
    pub packets_lost: u64,
}

impl UdpIntervalProgress {
    /// Late gap repairs increase received without advancing the expected
    /// denominator. Compare both monotonic coordinates so an old snapshot
    /// cannot undo a repair, even when it traveled over a different channel.
    pub fn is_at_least_as_fresh_as(&self, previous: &Self) -> bool {
        self.packets_received >= previous.packets_received
            && self.packets_received.saturating_add(self.packets_lost)
                >= previous
                    .packets_received
                    .saturating_add(previous.packets_lost)
    }

    /// Cumulative loss percent. Returns `None` when no packets have been seen
    /// yet (denominator zero) so callers can distinguish "no traffic" from a
    /// real 0.0% reading.
    pub fn lost_percent(&self) -> Option<f64> {
        let denom = self.packets_received.saturating_add(self.packets_lost);
        if denom == 0 {
            None
        } else {
            Some((self.packets_lost as f64 / denom as f64) * 100.0)
        }
    }
}

/// Receiver measurements for one client-relative UDP direction.
///
/// Counts are cumulative across the test. A missing jitter reading means
/// unknown (for example, receiver feedback carries counts only); a measured
/// zero jitter remains `Some(0.0)`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
pub struct UdpDirectionProgress {
    #[serde(flatten)]
    pub progress: UdpIntervalProgress,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jitter_ms: Option<f64>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtt_us: Option<u32>,
    /// Congestion window in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwnd: Option<u32>,
    /// Bidirectional interval upload bytes, from the client's perspective.
    /// Absent for unidirectional tests and peers without direction splits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_sent: Option<u64>,
    /// Bidirectional interval download bytes, from the client's perspective.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_received: Option<u64>,
    /// Upload throughput over the same interval window as `bytes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throughput_send_mbps: Option<f64>,
    /// Download throughput over the same interval window as `bytes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throughput_recv_mbps: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateInterval {
    pub bytes: u64,
    pub throughput_mbps: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retransmits: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jitter_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lost: Option<u64>,
    /// Cumulative UDP packet-progress snapshot as of this interval (raw
    /// counts, not a derived percent). Lets the client compute percent itself
    /// and treat absence as "unknown" for a freshness signal in the TUI; also
    /// keeps the schema composable for future per-direction loss reporting.
    /// Pre-0.9.11 servers do not emit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udp_progress: Option<UdpIntervalProgress>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rtt_us: Option<u32>,
    /// Sum of per-stream congestion windows in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwnd: Option<u32>,
    /// Bidirectional test: upload bytes from the client's perspective during this interval.
    /// Populated only for bidir tests; None for unidirectional (where `bytes` is authoritative).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_sent: Option<u64>,
    /// Bidirectional test: download bytes from the client's perspective during this interval.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_received: Option<u64>,
    /// Bidirectional test: per-direction throughput (upload) in Mbps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throughput_send_mbps: Option<f64>,
    /// Bidirectional test: per-direction throughput (download) in Mbps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throughput_recv_mbps: Option<f64>,
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
    /// UDP upload measurements from the server's receiver, from the client's
    /// perspective. Absent when unavailable, including older peer results.
    /// Directional aggregates are boxed so every `ControlMessage` does not
    /// reserve inline space for two receiver snapshots; JSON is unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udp_stats_send: Option<Box<UdpStats>>,
    /// UDP download measurements from the client's receiver. Filled locally
    /// after receive tasks finish; the server cannot measure this direction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub udp_stats_recv: Option<Box<UdpStats>>,
    /// Bidirectional test: total upload bytes from the client's perspective.
    /// Populated only for bidir tests; None for unidirectional (where `bytes_total` is authoritative).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_sent: Option<u64>,
    /// Bidirectional test: total download bytes from the client's perspective.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_received: Option<u64>,
    /// Bidirectional test: per-direction throughput (upload) in Mbps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throughput_send_mbps: Option<f64>,
    /// Bidirectional test: per-direction throughput (download) in Mbps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throughput_recv_mbps: Option<f64>,
    /// Path-MTU probe report (issue #64). Populated client-side after a
    /// `--probe-mtu` run — the client is the only side with the full
    /// per-size picture — so it rides into `--json` output. The server
    /// never sets it; wire-additive, absent for throughput tests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mtu_probe: Option<crate::probe::MtuProbeReport>,
    /// UDP datagrams the server failed to send mid-test (download, or the
    /// server half of bidir). Wire-additive: absent when there were none,
    /// and older servers never send it; older clients ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_udp_send_errors: Option<UdpSendErrors>,
    /// UDP datagrams the client failed to send mid-test (upload, or the
    /// client half of bidir). Populated client-side only, like `mtu_probe`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_udp_send_errors: Option<UdpSendErrors>,
}

/// Count of failed UDP sends plus the most recent error message. Errors in
/// the teardown window (see `udp::is_teardown_send_error`) are not counted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UdpSendErrors {
    pub count: u64,
    pub last_error: String,
}

impl UdpSendErrors {
    /// Longest `last_error` kept; longer messages are truncated.
    pub const MAX_ERROR_LEN: usize = 128;

    /// Strip control characters and bound the length. Applied where the
    /// message is recorded and again on the client, since a server's
    /// message crosses a trust boundary before reaching the terminal.
    pub fn sanitize_message(raw: &str) -> String {
        raw.chars()
            .filter(|c| !c.is_control())
            .take(Self::MAX_ERROR_LEN)
            .collect()
    }

    /// One-line human summary, e.g. `Server reported 3 UDP send errors (last: ...)`.
    pub fn describe(&self, side: &str) -> String {
        format!(
            "{} reported {} UDP send error{} (last: {})",
            side,
            self.count,
            if self.count == 1 { "" } else { "s" },
            self.last_error
        )
    }
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
    /// Bidirectional upload bytes, from the client's perspective.
    /// Absent for unidirectional tests and peers without direction splits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_sent: Option<u64>,
    /// Bidirectional download bytes, from the client's perspective.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_received: Option<u64>,
    /// Upload throughput over the result's test duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throughput_send_mbps: Option<f64>,
    /// Download throughput over the result's test duration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub throughput_recv_mbps: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TcpInfoSnapshot {
    pub retransmits: u64,
    pub rtt_us: u32,
    pub rtt_var_us: u32,
    /// Congestion window in bytes.
    pub cwnd: u32,
    /// Bytes acknowledged by the peer (from `tcpi_bytes_acked` on Linux).
    /// Used to correct overcount on abortive close where unACK'd buffer is discarded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_acked: Option<u64>,
}

/// Receiver-side UDP measurements. `packets_sent` is the expected sequence
/// count inferred from received packets and gaps, rather than a sender counter;
/// packets lost entirely beyond the observed sequence range are unknown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UdpStats {
    /// Receiver-inferred expected count through the highest sequence seen.
    /// Packets lost after that sequence cannot be inferred by the receiver.
    pub packets_sent: u64,
    /// Unique packets accepted by the receiver's bounded sequence window.
    pub packets_received: u64,
    pub lost: u64,
    pub lost_percent: f64,
    pub jitter_ms: f64,
    /// Unique late packets within the sequence window, excluding duplicates.
    pub out_of_order: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jitter_max_ms: Option<f64>,
    /// Largest valid UDP data payload received; absent when no data arrived.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub packet_size: Option<u32>,
}

/// Timestamp format options for interval output
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash, Default)]
#[serde(rename_all = "snake_case")]
pub enum TimestampFormat {
    /// Seconds since test start (default): [5.2s]
    #[default]
    Relative,
    /// ISO 8601 format: 2026-01-31T21:45:30Z
    Iso8601,
    /// Unix epoch with milliseconds: 1738356330.123
    Unix,
}

impl TimestampFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Relative => "relative",
            Self::Iso8601 => "iso8601",
            Self::Unix => "unix",
        }
    }

    pub fn next(&self) -> Self {
        match self {
            Self::Relative => Self::Iso8601,
            Self::Iso8601 => Self::Unix,
            Self::Unix => Self::Relative,
        }
    }

    pub fn prev(&self) -> Self {
        match self {
            Self::Relative => Self::Unix,
            Self::Iso8601 => Self::Relative,
            Self::Unix => Self::Iso8601,
        }
    }

    /// Format a timestamp based on the format type
    ///
    /// # Arguments
    /// * `test_start` - When the test started (monotonic, for relative calculation)
    /// * `now` - Current monotonic time
    /// * `system_start` - Wall clock time when test started (for ISO8601/Unix)
    pub fn format(
        &self,
        test_start: std::time::Instant,
        now: std::time::Instant,
        system_start: std::time::SystemTime,
    ) -> String {
        match self {
            TimestampFormat::Relative => {
                let elapsed = now.duration_since(test_start);
                format!("{:.3}", elapsed.as_secs_f64())
            }
            TimestampFormat::Iso8601 => {
                let elapsed = now.duration_since(test_start);
                let wall_time = system_start + elapsed;
                let datetime = chrono::DateTime::<chrono::Utc>::from(wall_time);
                datetime.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
            }
            TimestampFormat::Unix => {
                let elapsed = now.duration_since(test_start);
                let wall_time = system_start + elapsed;
                let duration_since_epoch = wall_time
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default();
                format!("{:.3}", duration_since_epoch.as_secs_f64())
            }
        }
    }

    /// Format for CLI help text
    pub fn variants() -> &'static [&'static str] {
        &["relative", "iso8601", "unix"]
    }
}

impl std::str::FromStr for TimestampFormat {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "relative" => Ok(TimestampFormat::Relative),
            "iso8601" | "iso" => Ok(TimestampFormat::Iso8601),
            "unix" | "epoch" => Ok(TimestampFormat::Unix),
            _ => Err(format!(
                "Invalid timestamp format: {}. Valid options: {}",
                s,
                Self::variants().join(", ")
            )),
        }
    }
}

impl std::fmt::Display for TimestampFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Capability strings advertised in client and server hello messages.
/// Both peers must list a string here for the corresponding feature to
/// be active in the session. Optional capabilities are wire-additive and
/// fall back to the prior behavior with an older peer. Capabilities marked
/// mandatory below instead fail closed when their protected operation is
/// requested; they must never silently downgrade.
pub const SUPPORTED_CAPABILITIES: &[&str] = &[
    "tcp",
    "udp",
    "quic",
    "multistream",
    "single_port_tcp",
    "pause_resume",
    // v0.9.14: server emits cumulative receiver loss back to client over
    // UDP at 2 Hz when both peers advertise this. Sidesteps TCP control
    // for live UDP-loss visibility under upload-mode saturation.
    "udp_feedback_v1",
    // v0.9.15: server honors TestStart.zerocopy (sendfile-based sends for
    // download/bidir TCP, issue #33). Advertised so clients can warn when
    // a `-R --zerocopy` request lands on a server that will ignore it.
    "zerocopy_v1",
    // v0.9.17: server answers XFRP path-MTU probe packets (ack + echo)
    // when TestStart.mtu_probe is set (issue #64). Hard requirement for
    // --probe-mtu — without it the client refuses to start, since an old
    // server would silently count probes as malformed data packets.
    "mtu_probe_v1",
    // Issue #63: all per-stream UDP data rides connected sockets bound to
    // the server's main port instead of per-stream ephemeral ports, so a
    // firewall only needs one UDP port open. The client always advertises
    // it; the server advertises it only when its startup self-test proved
    // the kernel routes connected-socket traffic past the shared wildcard
    // socket (see net::single_port_udp_self_test). Default when both
    // peers advertise, mirroring single_port_tcp.
    SINGLE_PORT_UDP_CAPABILITY,
    // v0.9.20: PSK sessions require AEAD-protected control channel.
    // Mandatory when PSK is configured — not a negotiable fallback.
    "protected_control_v1",
    // PSK-authenticated QUIC sessions mix RFC 9266 TLS exporter material into
    // every application-layer auth and control key. Mandatory for QUIC + PSK
    // so the application handshake cannot be relayed across two TLS sessions.
    QUIC_CHANNEL_BINDING_CAPABILITY,
    // v0.9.23: server honors TestStart.byte_budget and the Finish message
    // (`-n`, transfer a fixed number of bytes instead of running for a
    // fixed time). Hard requirement for `-n` — an old server ignores the
    // field and runs a timed test, which would silently answer a different
    // question than the one asked.
    BYTE_BUDGET_CAPABILITY,
    UDP_PAYLOAD_SIZE_CAPABILITY,
];

/// Capability for honoring explicit UDP payload sizes (`-l/--length`).
pub const UDP_PAYLOAD_SIZE_CAPABILITY: &str = "udp_payload_size_v1";

/// Capability string for byte-budget tests (`-n`). Named because both the
/// client's pre-flight check and the server's hello reference it.
pub const BYTE_BUDGET_CAPABILITY: &str = "byte_budget_v1";

/// Mandatory capability for PSK-authenticated QUIC sessions to bind the PSK
/// handshake and protected control channel to the underlying TLS connection.
pub const QUIC_CHANNEL_BINDING_CAPABILITY: &str = "quic_channel_binding_v1";

/// Capability string for single-port UDP (issue #63). Kept as a named
/// constant because the server filters it out of its hello at runtime
/// when the startup self-test fails.
pub const SINGLE_PORT_UDP_CAPABILITY: &str = "single_port_udp_v1";

pub fn supported_capabilities() -> Vec<String> {
    SUPPORTED_CAPABILITIES
        .iter()
        .map(|s| s.to_string())
        .collect()
}

/// The full capability list minus one entry. Used by the server to
/// suppress runtime-gated capabilities (currently only
/// [`SINGLE_PORT_UDP_CAPABILITY`]) without duplicating the list.
pub fn supported_capabilities_without(excluded: &str) -> Vec<String> {
    SUPPORTED_CAPABILITIES
        .iter()
        .filter(|c| **c != excluded)
        .map(|s| s.to_string())
        .collect()
}

/// Returns true if the peer's hello capability vec includes
/// `udp_feedback_v1`. Used by the server to decide whether to spawn
/// the feedback emission task and by the client to decide whether to
/// run the feedback receive task.
pub fn capability_advertised(capabilities: &Option<Vec<String>>, name: &str) -> bool {
    capabilities
        .as_ref()
        .map(|caps| caps.iter().any(|c| c == name))
        .unwrap_or(false)
}

impl ControlMessage {
    pub fn client_hello() -> Self {
        ControlMessage::Hello {
            version: PROTOCOL_VERSION.to_string(),
            client: Some(format!("xfr/{}", env!("CARGO_PKG_VERSION"))),
            server: None,
            capabilities: Some(supported_capabilities()),
            auth: None,
            client_nonce: None,
        }
    }

    /// Client hello with a PSK-derived nonce for protected control.
    pub fn client_hello_with_nonce(client_nonce: String) -> Self {
        ControlMessage::Hello {
            version: PROTOCOL_VERSION.to_string(),
            client: Some(format!("xfr/{}", env!("CARGO_PKG_VERSION"))),
            server: None,
            capabilities: Some(supported_capabilities()),
            auth: None,
            client_nonce: Some(client_nonce),
        }
    }

    pub fn server_hello() -> Self {
        Self::server_hello_with_capabilities(supported_capabilities())
    }

    /// Server hello with a runtime-determined capability list (the static
    /// list minus capabilities whose startup self-tests failed).
    pub fn server_hello_with_capabilities(capabilities: Vec<String>) -> Self {
        ControlMessage::Hello {
            version: PROTOCOL_VERSION.to_string(),
            client: None,
            server: Some(format!("xfr/{}", env!("CARGO_PKG_VERSION"))),
            capabilities: Some(capabilities),
            auth: None,
            client_nonce: None,
        }
    }

    pub fn server_hello_with_auth(nonce: String) -> Self {
        Self::server_hello_with_auth_and_capabilities(nonce, supported_capabilities())
    }

    pub fn server_hello_with_auth_and_capabilities(
        nonce: String,
        capabilities: Vec<String>,
    ) -> Self {
        ControlMessage::Hello {
            version: PROTOCOL_VERSION.to_string(),
            client: None,
            server: Some(format!("xfr/{}", env!("CARGO_PKG_VERSION"))),
            capabilities: Some(capabilities),
            auth: Some(AuthChallenge {
                method: "psk".to_string(),
                nonce,
            }),
            client_nonce: None,
        }
    }

    pub fn auth_response(response: String) -> Self {
        ControlMessage::AuthResponse { response }
    }

    pub fn auth_success() -> Self {
        ControlMessage::AuthSuccess { server_proof: None }
    }

    pub fn auth_success_with_proof(server_proof: String) -> Self {
        ControlMessage::AuthSuccess {
            server_proof: Some(server_proof),
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        ControlMessage::Error {
            message: message.into(),
        }
    }

    pub fn serialize(&self) -> anyhow::Result<String> {
        Ok(serde_json::to_string(self)?)
    }

    pub fn deserialize(s: &str) -> anyhow::Result<Self> {
        let msg: Self = serde_json::from_str(s)?;
        msg.validate()?;
        Ok(msg)
    }
}

impl ControlMessage {
    /// Post-deserialization validation for wire messages.
    fn validate(&self) -> anyhow::Result<()> {
        if let Self::TestAck {
            data_ports,
            udp_token,
            ..
        } = self
        {
            // Single-port UDP mode and legacy multi-port UDP are
            // mutually exclusive: a token is only sent when no per-stream
            // ports are allocated.
            if udp_token.is_some() && !data_ports.is_empty() {
                anyhow::bail!("TestAck cannot contain both udp_token and data_ports");
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directional_udp_progress_flattens_counts_and_preserves_unknown_jitter() {
        let counts = serde_json::json!({"packets_received": 9, "packets_lost": 1});
        let mut progress: UdpDirectionProgress = serde_json::from_value(counts.clone()).unwrap();
        assert_eq!(progress.progress.lost_percent(), Some(10.0));
        assert_eq!(progress.jitter_ms, None);
        assert_eq!(serde_json::to_value(progress).unwrap(), counts);

        // A measured zero is different from a counts-only feedback reading.
        progress.jitter_ms = Some(0.0);
        let measured = serde_json::to_value(progress).unwrap();
        assert_eq!(
            measured,
            serde_json::json!({"packets_received": 9, "packets_lost": 1, "jitter_ms": 0.0})
        );
        let decoded: UdpDirectionProgress = serde_json::from_value(measured).unwrap();
        assert_eq!(decoded.jitter_ms, Some(0.0));
    }

    #[test]
    fn legacy_stream_reports_omit_direction_splits() {
        let interval = serde_json::json!({"id": 3, "bytes": 1000});
        let decoded: StreamInterval = serde_json::from_value(interval.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), interval);

        let result = serde_json::json!({"id": 3, "bytes": 1000, "throughput_mbps": 0.008});
        let decoded: StreamResult = serde_json::from_value(result.clone()).unwrap();
        assert_eq!(serde_json::to_value(decoded).unwrap(), result);

        let enriched = serde_json::json!({
            "id": 3, "bytes": 1000,
            "bytes_sent": 750, "bytes_received": 250,
            "throughput_send_mbps": 0.006, "throughput_recv_mbps": 0.002
        });
        let decoded: StreamInterval = serde_json::from_value(enriched.clone()).unwrap();
        assert_eq!(decoded.bytes_sent, Some(750));
        assert_eq!(decoded.bytes_received, Some(250));
        assert_eq!(serde_json::to_value(decoded).unwrap(), enriched);
    }

    #[test]
    fn directional_results_roundtrip_and_preserve_legacy_payload() {
        let legacy = serde_json::json!({
            "id": "bidir", "bytes_total": 1000, "duration_ms": 1000,
            "throughput_mbps": 0.008,
            "streams": [{"id": 3, "bytes": 1000, "throughput_mbps": 0.008}],
            "udp_stats": {
                "packets_sent": 100, "packets_received": 99,
                "lost": 1, "lost_percent": 1.0, "jitter_ms": 0.25,
                "out_of_order": 0
            }
        });
        let mut result: TestResult = serde_json::from_value(legacy.clone()).unwrap();
        assert!(result.udp_stats_send.is_none());
        assert!(result.udp_stats_recv.is_none());
        assert_eq!(serde_json::to_value(&result).unwrap(), legacy);

        result.udp_stats_send = result.udp_stats.clone().map(Box::new);
        result.udp_stats_recv = Some(Box::new(UdpStats {
            packets_sent: 100,
            packets_received: 75,
            lost: 25,
            lost_percent: 25.0,
            jitter_ms: 2.5,
            out_of_order: 2,
            jitter_max_ms: Some(5.0),
            packet_size: Some(1400),
        }));
        let stream = &mut result.streams[0];
        stream.bytes_sent = Some(750);
        stream.bytes_received = Some(250);
        stream.throughput_send_mbps = Some(0.006);
        stream.throughput_recv_mbps = Some(0.002);

        let enriched = ControlMessage::Result(result).serialize().unwrap();
        let ControlMessage::Result(decoded) = ControlMessage::deserialize(&enriched).unwrap()
        else {
            panic!("expected result");
        };
        assert_eq!(decoded.udp_stats_send.unwrap().lost, 1);
        assert_eq!(decoded.udp_stats_recv.unwrap().lost, 25);
        assert_eq!(decoded.streams[0].bytes_sent, Some(750));
        assert_eq!(decoded.streams[0].throughput_recv_mbps, Some(0.002));

        // Dropping only the additive fields leaves exactly the older payload.
        let mut projection: serde_json::Value = serde_json::from_str(&enriched).unwrap();
        let object = projection.as_object_mut().unwrap();
        object.remove("type");
        object.remove("udp_stats_send");
        object.remove("udp_stats_recv");
        for stream in projection["streams"].as_array_mut().unwrap() {
            for field in [
                "bytes_sent",
                "bytes_received",
                "throughput_send_mbps",
                "throughput_recv_mbps",
            ] {
                stream.as_object_mut().unwrap().remove(field);
            }
        }
        assert_eq!(projection, legacy);
    }

    #[test]
    fn test_serialize_hello() {
        let msg = ControlMessage::client_hello();
        let json = msg.serialize().unwrap();
        assert!(json.contains("\"type\":\"hello\""));
        assert!(json.contains("\"version\":\"1.1\""));
    }

    #[test]
    fn test_versions_compatible_matches_major_versions() {
        assert!(versions_compatible("1.1", "1.0"));
        assert!(versions_compatible("1.1", "1.99"));
        assert!(versions_compatible("1.1.2", "1.0.0"));
        assert!(versions_compatible("1", "1.1"));
        assert!(!versions_compatible("1.1", "2.0"));
    }

    #[test]
    fn test_versions_compatible_rejects_malformed_versions() {
        for bad in ["", ".", "x.1", "1.x", ".1", "1.", "1.1.x", "1..1"] {
            assert!(
                !versions_compatible(bad, PROTOCOL_VERSION),
                "{bad:?} must not be compatible as the peer version"
            );
            assert!(
                !versions_compatible(PROTOCOL_VERSION, bad),
                "{bad:?} must not be compatible as the local version"
            );
        }
    }

    #[test]
    fn test_roundtrip_test_start() {
        let msg = ControlMessage::TestStart {
            id: "test123".to_string(),
            protocol: Protocol::Tcp,
            streams: 4,
            duration_secs: 10,
            direction: Direction::Upload,
            bitrate: None,
            congestion: None,
            mptcp: false,
            dscp: None,
            window_size: None,
            zerocopy: false,
            mtu_probe: false,
            tcp_nodelay: false,
            byte_budget: None,
            udp_payload_size: None,
        };
        let json = msg.serialize().unwrap();
        let decoded = ControlMessage::deserialize(&json).unwrap();

        match decoded {
            ControlMessage::TestStart { id, streams, .. } => {
                assert_eq!(id, "test123");
                assert_eq!(streams, 4);
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_test_start_backward_compat_without_window_size() {
        // Simulate an older client that doesn't know about window_size: the JSON
        // omits the field entirely. The new server must deserialize it as None
        // and fall back to kernel autotuning.
        let json = r#"{"type":"test_start","id":"x","protocol":"tcp","streams":1,"duration_secs":5,"direction":"upload"}"#;
        let decoded = ControlMessage::deserialize(json).unwrap();
        match decoded {
            ControlMessage::TestStart {
                window_size, dscp, ..
            } => {
                assert!(
                    window_size.is_none(),
                    "absent field must deserialize to None"
                );
                assert!(dscp.is_none(), "sanity: dscp also absent");
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_test_start_roundtrip_with_window_size() {
        let msg = ControlMessage::TestStart {
            id: "x".to_string(),
            protocol: Protocol::Tcp,
            streams: 1,
            duration_secs: 5,
            direction: Direction::Upload,
            bitrate: None,
            congestion: None,
            mptcp: false,
            dscp: None,
            window_size: Some(262_144),
            zerocopy: false,
            mtu_probe: false,
            tcp_nodelay: false,
            byte_budget: None,
            udp_payload_size: None,
        };
        let json = msg.serialize().unwrap();
        assert!(json.contains("\"window_size\":262144"));
        let decoded = ControlMessage::deserialize(&json).unwrap();
        match decoded {
            ControlMessage::TestStart { window_size, .. } => {
                assert_eq!(window_size, Some(262_144));
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_test_start_window_size_above_u32_max_roundtrips() {
        // u64 wire type must round-trip values that would silently truncate in
        // u32 (e.g. `-w 4G` on a 64-bit client becomes 4_294_967_296 = u32::MAX+1).
        let big = u32::MAX as u64 + 1;
        let msg = ControlMessage::TestStart {
            id: "x".to_string(),
            protocol: Protocol::Tcp,
            streams: 1,
            duration_secs: 5,
            direction: Direction::Upload,
            bitrate: None,
            congestion: None,
            mptcp: false,
            dscp: None,
            window_size: Some(big),
            zerocopy: false,
            mtu_probe: false,
            tcp_nodelay: false,
            byte_budget: None,
            udp_payload_size: None,
        };
        let json = msg.serialize().unwrap();
        let decoded = ControlMessage::deserialize(&json).unwrap();
        match decoded {
            ControlMessage::TestStart { window_size, .. } => {
                assert_eq!(window_size, Some(big));
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_test_start_serialize_omits_none_window_size() {
        // skip_serializing_if = Option::is_none must keep the field out of the
        // JSON entirely when None, so older deserializers don't fail on it.
        let msg = ControlMessage::TestStart {
            id: "x".to_string(),
            protocol: Protocol::Tcp,
            streams: 1,
            duration_secs: 5,
            direction: Direction::Upload,
            bitrate: None,
            congestion: None,
            mptcp: false,
            dscp: None,
            window_size: None,
            zerocopy: false,
            mtu_probe: false,
            tcp_nodelay: false,
            byte_budget: None,
            udp_payload_size: None,
        };
        let json = msg.serialize().unwrap();
        assert!(
            !json.contains("window_size"),
            "None must be skipped in serialization, got: {}",
            json
        );
        assert!(
            !json.contains("zerocopy"),
            "false zerocopy must be skipped in serialization, got: {}",
            json
        );
        assert!(
            !json.contains("tcp_nodelay"),
            "false tcp_nodelay must be skipped in serialization, got: {}",
            json
        );
    }

    #[test]
    fn test_test_start_backward_compat_without_tcp_nodelay() {
        // Older peers don't send the tcp_nodelay field; it must default to false.
        let json = r#"{"type":"test_start","id":"x","protocol":"tcp","streams":1,"duration_secs":5,"direction":"download"}"#;
        let decoded = ControlMessage::deserialize(json).unwrap();
        match decoded {
            ControlMessage::TestStart { tcp_nodelay, .. } => {
                assert!(!tcp_nodelay, "absent field must deserialize to false");
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_test_start_roundtrip_with_tcp_nodelay() {
        let msg = ControlMessage::TestStart {
            id: "x".to_string(),
            protocol: Protocol::Tcp,
            streams: 1,
            duration_secs: 5,
            direction: Direction::Download,
            bitrate: None,
            congestion: None,
            mptcp: false,
            dscp: None,
            window_size: None,
            zerocopy: false,
            mtu_probe: false,
            tcp_nodelay: true,
            byte_budget: None,
            udp_payload_size: None,
        };
        let json = msg.serialize().unwrap();
        assert!(json.contains("\"tcp_nodelay\":true"));
        let decoded = ControlMessage::deserialize(&json).unwrap();
        match decoded {
            ControlMessage::TestStart { tcp_nodelay, .. } => {
                assert!(tcp_nodelay);
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_test_start_backward_compat_without_zerocopy() {
        // Older peers don't send the zerocopy field; it must default to false.
        let json = r#"{"type":"test_start","id":"x","protocol":"tcp","streams":1,"duration_secs":5,"direction":"download"}"#;
        let decoded = ControlMessage::deserialize(json).unwrap();
        match decoded {
            ControlMessage::TestStart { zerocopy, .. } => {
                assert!(!zerocopy, "absent field must deserialize to false");
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_test_start_roundtrip_with_zerocopy() {
        let msg = ControlMessage::TestStart {
            id: "x".to_string(),
            protocol: Protocol::Tcp,
            streams: 1,
            duration_secs: 5,
            direction: Direction::Download,
            bitrate: None,
            congestion: None,
            mptcp: false,
            dscp: None,
            window_size: None,
            zerocopy: true,
            mtu_probe: false,
            tcp_nodelay: false,
            byte_budget: None,
            udp_payload_size: None,
        };
        let json = msg.serialize().unwrap();
        assert!(json.contains("\"zerocopy\":true"));
        let decoded = ControlMessage::deserialize(&json).unwrap();
        match decoded {
            ControlMessage::TestStart { zerocopy, .. } => {
                assert!(zerocopy);
            }
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_protocol_display() {
        assert_eq!(Protocol::Tcp.to_string(), "TCP");
        assert_eq!(Protocol::Udp.to_string(), "UDP");
    }

    #[test]
    fn test_test_ack_udp_token_roundtrip_and_omission() {
        let msg = ControlMessage::TestAck {
            id: "x".to_string(),
            data_ports: vec![],
            udp_token: Some("00112233445566778899aabbccddeeff".to_string()),
        };
        let json = msg.serialize().unwrap();
        assert!(json.contains("\"udp_token\""));
        match ControlMessage::deserialize(&json).unwrap() {
            ControlMessage::TestAck { udp_token, .. } => {
                assert_eq!(
                    udp_token.as_deref(),
                    Some("00112233445566778899aabbccddeeff")
                );
            }
            _ => panic!("wrong message type"),
        }

        // None must be omitted so pre-#63 deserializers never see the field,
        // and a legacy TestAck without it must decode as None.
        let msg = ControlMessage::TestAck {
            id: "x".to_string(),
            data_ports: vec![5202],
            udp_token: None,
        };
        let json = msg.serialize().unwrap();
        assert!(!json.contains("udp_token"));
        let legacy = r#"{"type":"test_ack","id":"x","data_ports":[5202]}"#;
        match ControlMessage::deserialize(legacy).unwrap() {
            ControlMessage::TestAck { udp_token, .. } => assert!(udp_token.is_none()),
            _ => panic!("wrong message type"),
        }
    }

    #[test]
    fn test_supported_capabilities_single_port_udp() {
        let full = supported_capabilities();
        assert!(full.iter().any(|c| c == SINGLE_PORT_UDP_CAPABILITY));
        let filtered = supported_capabilities_without(SINGLE_PORT_UDP_CAPABILITY);
        assert!(!filtered.iter().any(|c| c == SINGLE_PORT_UDP_CAPABILITY));
        assert_eq!(filtered.len(), full.len() - 1);
    }

    #[test]
    fn test_supported_capabilities_quic_channel_binding() {
        assert!(
            supported_capabilities()
                .iter()
                .any(|c| c == QUIC_CHANNEL_BINDING_CAPABILITY)
        );
    }

    #[test]
    fn test_test_ack_rejects_udp_token_with_data_ports() {
        let bad = r#"{"type":"test_ack","id":"x","data_ports":[5202],"udp_token":"00112233445566778899aabbccddeeff"}"#;
        let result = ControlMessage::deserialize(bad);
        assert!(
            result.is_err(),
            "TestAck with both udp_token and data_ports should be rejected"
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("udp_token") && err.contains("data_ports"),
            "{}",
            err
        );
    }

    #[test]
    fn test_test_ack_accepts_data_ports_without_udp_token() {
        let good = r#"{"type":"test_ack","id":"x","data_ports":[5202]}"#;
        let result = ControlMessage::deserialize(good);
        assert!(result.is_ok(), "legacy multi-port TestAck should parse");
    }

    #[test]
    fn test_test_ack_accepts_udp_token_with_empty_data_ports() {
        let good = r#"{"type":"test_ack","id":"x","data_ports":[],"udp_token":"00112233445566778899aabbccddeeff"}"#;
        let result = ControlMessage::deserialize(good);
        assert!(result.is_ok(), "single-port TestAck should parse");
    }
}

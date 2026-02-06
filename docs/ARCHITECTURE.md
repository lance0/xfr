# Architecture

This document describes xfr's internal architecture for contributors.

## Module Structure

```
src/
├── main.rs          # CLI entry point, argument parsing (clap)
├── lib.rs           # Library root, public API exports
│
├── client.rs        # Client-side test orchestration
├── serve.rs         # Server-side connection handling
│
├── protocol.rs      # Control protocol (JSON messages)
├── tcp.rs           # TCP data transfer
├── udp.rs           # UDP data transfer with pacing
├── quic.rs          # QUIC transport (quinn)
├── net.rs           # Network utilities (address resolution, sockets)
│
├── stats.rs         # Statistics collection and aggregation
├── tcp_info.rs      # Platform-specific TCP_INFO extraction
│
├── config.rs        # Config file parsing
├── prefs.rs         # User preferences (theme)
│
├── auth.rs          # PSK authentication (HMAC-SHA256)
├── acl.rs           # IP access control lists
├── rate_limit.rs    # Per-IP rate limiting
│
├── diff.rs          # Result comparison
├── discover.rs      # mDNS discovery
├── update.rs        # GitHub release update notifications
│
├── output/          # Output formatters
│   ├── mod.rs
│   ├── json.rs
│   ├── csv.rs
│   ├── plain.rs
│   ├── prometheus.rs    # Prometheus metrics endpoint
│   └── push_gateway.rs # Push gateway integration
│
└── tui/             # Terminal UI
    ├── mod.rs
    ├── app.rs       # Application state and event loop
    ├── ui.rs        # Layout and rendering
    ├── server.rs    # Server dashboard
    ├── settings.rs  # Settings modal
    ├── widgets.rs   # Sparkline, progress bar
    └── theme.rs     # Color themes
```

## Data Flow

### Test Lifecycle

```
┌────────────────────────────────────────────────────────────────────┐
│ CLIENT                                                              │
│                                                                     │
│  1. Connect TCP to server:5201 (control channel)                   │
│  2. Send Hello (version 1.1, capabilities list)                     │
│     Capabilities: tcp, udp, quic, multistream, single_port_tcp      │
│  3. Receive server Hello (version, auth challenge if PSK)           │
│  4. If PSK: complete challenge-response auth                        │
│  5. Send TestStart (protocol, streams, duration, direction)         │
│  6. Receive TestAck (with data_ports: empty for single-port TCP)    │
│  7. Open data connections to port 5201, send DataHello              │
│     (single-port TCP is default for clients with that capability;   │
│      multi-port fallback for legacy clients without single_port_tcp)│
│  8. Transfer data, receive Interval messages                        │
│  9. Receive Result message                                          │
│ 10. Display results                                                  │
└────────────────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────────────────┐
│ SERVER                                                              │
│                                                                     │
│  1. Accept connection, spawn task via handle_new_connection()       │
│  2. Read first line (5s INITIAL_READ_TIMEOUT, resist slow-loris)   │
│  3. Parse message, route:                                           │
│     - If Hello: acquire rate-limit + semaphore, spawn client task   │
│       - Send Hello (version, auth challenge if PSK)                 │
│       - If PSK: verify auth response                                │
│       - Receive TestStart, send TestAck                             │
│     - If DataHello: validate test_id against active_tests,          │
│       verify peer IP matches control connection, route to test      │
│  4. Transfer data, send Interval messages                           │
│  5. Send Result message                                             │
│  6. Clean up, ready for next client                                 │
│     (one-off mode: signal shutdown via watch channel)               │
└────────────────────────────────────────────────────────────────────┘
```

### Control Protocol

The control channel uses newline-delimited JSON messages (one JSON object per line).
Protocol version is **1.1** (major version must match for compatibility).

Messages use a tagged enum format with a `"type"` field:

```
{"type":"hello","version":"1.1","client":"xfr/x.y.z","capabilities":["tcp","udp","quic","multistream","single_port_tcp"]}\n
{"type":"test_start","id":"...","protocol":"tcp","streams":4,...}\n
```

Each message is serialized as compact JSON followed by a newline character.
The receiver reads line-by-line and deserializes each line as a `ControlMessage`.

Message types (see `protocol.rs`):
- `hello` - Version and capability exchange (client sends capabilities list)
- `auth_response` - PSK authentication
- `auth_success` - Auth confirmation
- `test_start` - Test parameters
- `test_ack` - Test acknowledgment (includes data_ports, empty for single-port TCP)
- `data_hello` - Data connection identification (test_id, stream_index)
- `interval` - Periodic statistics
- `result` - Final test result
- `cancel` / `cancelled` - Test cancellation
- `error` - Error messages

### TCP Connection Modes

**Single-port TCP** (default for clients advertising `single_port_tcp` capability):
All data connections come to the same port as the control connection (5201).
Each data connection sends a `DataHello` as its first message to identify which
test and stream it belongs to. The server routes these via an mpsc channel to the
active test's stream handler.

**Multi-port TCP** (legacy fallback for clients without `single_port_tcp`):
The server allocates per-stream ephemeral TCP listeners and returns their ports
in the `TestAck` message. Clients connect to each port directly. No `DataHello`
routing is needed since the port uniquely identifies the stream.

### Data Transfer

**TCP**: Simple bulk send/receive with large buffers (128KB default, 4MB for high-speed mode).

**UDP**: Paced sending with sequence numbers for loss detection:
```
┌──────────┬──────────┬──────────────────────────────┐
│ seq (8B) │ ts (8B)  │ payload (variable)           │
└──────────┴──────────┴──────────────────────────────┘
```

**QUIC**: Stream 0 for control, streams 1-N for data:
```
Connection
├── Stream 0 (bidirectional): Control messages
├── Stream 1 (unidirectional): Data stream 1
├── Stream 2 (unidirectional): Data stream 2
└── ...
```

## Threading Model

xfr uses Tokio for async I/O. Key patterns:

### Server

The TCP accept loop spawns a task per connection to prevent slow-loris attacks
from blocking accepts:

```rust
// Main accept loop (uses select! with shutdown_rx for one-off mode)
loop {
    let (stream, peer_addr) = tokio::select! {
        result = listener.accept() => result?,
        _ = shutdown_rx.changed() => break, // one-off shutdown
    };
    // ACL check (inline, cheap)
    // Spawn task per connection for first-line read + routing
    tokio::spawn(handle_new_connection(stream, peer_addr, ...));
}

// handle_new_connection: reads first line (5s timeout), parses, routes
async fn handle_new_connection(...) {
    let line = timeout(INITIAL_READ_TIMEOUT, read_first_line(&mut stream)).await?;
    match parse(line) {
        DataHello { test_id, .. } => {
            // Validate test_id exists in active_tests, then route
            route_data_hello(stream, active_tests, test_id, stream_index).await;
        }
        Hello { .. } => {
            // Acquire rate-limit + semaphore, then spawn client handler
            tokio::spawn(handle_client_with_first_message(...));
        }
    }
}
```

The QUIC accept loop runs in a separate spawned task, also using `select!` with
a shared `watch` channel for shutdown signaling in one-off mode.

JoinErrors from stream handler `join_all` are logged (panics logged as errors).

```rust
// Per-client task spawns data handlers
async fn handle_client(...) {
    // Control channel handling
    // Spawn data transfer tasks via channel (single-port TCP)
    // or per-port listeners (multi-port TCP)

    // Wait for handlers, log JoinErrors
    let results = futures::future::join_all(handles).await;
    for result in results {
        if let Err(e) = result && e.is_panic() {
            error!("stream handler panicked: {:?}", e);
        }
    }
}
```

### Client

```rust
async fn run_test(...) {
    // Connect control channel
    // Exchange hello/auth
    // Send TestStart, receive TestAck

    // Spawn data transfer tasks
    let handles = spawn_data_transfers(...);

    // Stats aggregation loop
    let mut interval = tokio::time::interval(Duration::from_secs(1));
    loop {
        select! {
            _ = interval.tick() => {
                // Collect stats, update TUI
            }
            _ = cancel.changed() => break,
        }
    }
}
```

### TUI Updates

The TUI runs in its own task, receiving stats via channel:

```rust
let (tx, rx) = mpsc::channel(32);

// Stats task sends updates
tx.send(Stats { throughput, ... }).await;

// TUI task receives and renders
loop {
    select! {
        Some(stats) = rx.recv() => {
            terminal.draw(|f| render(f, &stats))?;
        }
        Some(key) = events.next() => {
            handle_key(key);
        }
    }
}
```

## Key Data Structures

### TestResult (protocol.rs)

```rust
pub struct TestResult {
    pub id: String,
    pub bytes_total: u64,
    pub duration_ms: u64,
    pub throughput_mbps: f64,
    pub streams: Vec<StreamResult>,
    pub tcp_info: Option<TcpInfoSnapshot>,   // RTT, retransmits, cwnd
    pub udp_stats: Option<UdpStats>,         // loss, jitter, out-of-order
}
```

### StreamStats (stats.rs)

```rust
pub struct StreamStats {
    pub stream_id: u8,
    pub bytes_sent: AtomicU64,
    pub bytes_received: AtomicU64,
    pub start_time: Instant,
    pub intervals: Mutex<VecDeque<IntervalStats>>,
    pub retransmits: AtomicU64,
    pub last_bytes: AtomicU64,
    pub last_retransmits: AtomicU64,
    // UDP stats (updated live by receiver)
    pub udp_jitter_us: AtomicU64,
    pub udp_lost: AtomicU64,
    pub last_udp_lost: AtomicU64,
    // Raw fd for live TCP_INFO polling (-1 = not set)
    pub tcp_info_fd: AtomicI32,
}
```

### ClientConfig / ServerConfig

See `client.rs` and `serve.rs` for configuration structs that hold all test parameters.

## Platform Specifics

### TCP_INFO

Platform-specific extraction of TCP statistics:

- **Linux**: `getsockopt(TCP_INFO)` with full stats
- **macOS**: `getsockopt(TCP_CONNECTION_INFO)` with partial stats
- **Windows**: Not implemented (no equivalent)

See `tcp_info.rs` for implementations.

### Live TCP_INFO Polling

TCP_INFO is polled live during tests, not just captured at the end. Each
`StreamStats` stores the raw file descriptor (`tcp_info_fd: AtomicI32`) so the
interval loop can call `getsockopt(TCP_INFO)` without holding a reference to
the `TcpStream` (which may be moved or split for bidir mode).

Lifecycle:
1. **Set** — after accepting/connecting the data socket, before transfer starts
2. **Poll** — `record_interval()` calls `poll_tcp_info()` each second to capture RTT/cwnd
3. **Clear** — at the end of each stream handler task (including early-return error paths)
   to prevent stale fd reuse if the OS reassigns the descriptor

### Congestion Control Validation

The `--congestion` algorithm is validated early, before any streams are spawned:
- **Client**: `tcp::validate_congestion()` probes the kernel with a temporary socket
  before sending `TestStart`. Invalid algorithms exit immediately with a helpful
  error listing available algorithms from `/proc/sys/net/ipv4/tcp_available_congestion_control`.
- **Server**: Same validation in `handle_test_request()` sends `ControlMessage::Error`
  back to the client (same pattern as stream count validation).

### Socket Buffers

Large buffers are used for high throughput:

```rust
// TCP (default, auto-bumps to 4MB for unlimited bitrate)
socket.set_send_buffer_size(131072)?;   // 128KB
socket.set_recv_buffer_size(131072)?;

// UDP (larger for burst tolerance)
socket.set_send_buffer_size(4194304)?;  // 4MB
socket.set_recv_buffer_size(4194304)?;
```

**Memory footprint:** Each stream allocates 128KB-4MB for buffers. A test with `-P 8` uses ~1-32MB; 10 concurrent clients with 8 streams each could use 80-320MB.

## Error Handling

- Use `anyhow::Result` for most functions
- Custom error types for protocol errors
- Graceful degradation (e.g., missing TCP_INFO)
- Proper cleanup on cancellation (Ctrl+C)

## Testing

```bash
cargo test                    # Unit tests
cargo test --test integration # Integration tests

# Integration tests spawn real server/client processes
# See tests/integration.rs
```

## Performance Considerations

1. **Minimize allocations in hot path** - Reuse buffers for data transfer
2. **Batch stats updates** - Don't lock on every packet
3. **TUI throttling** - Limit redraws to reduce CPU usage
4. **Async I/O** - Non-blocking throughout
5. **Large buffers** - Reduce syscall overhead

## Contributing

See [CONTRIBUTING.md](../CONTRIBUTING.md) for development workflow.

Key areas for contribution:
- New transport protocols
- Additional output formats
- Platform-specific optimizations
- TUI improvements

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
│
├── output/          # Output formatters
│   ├── mod.rs
│   ├── json.rs
│   ├── csv.rs
│   └── plain.rs
│
└── tui/             # Terminal UI
    ├── mod.rs
    ├── client.rs    # Client TUI
    ├── server.rs    # Server dashboard
    └── theme.rs     # Color themes
```

## Data Flow

### Test Lifecycle

```
┌────────────────────────────────────────────────────────────────────┐
│ CLIENT                                                              │
│                                                                     │
│  1. Connect TCP to server:5201 (control channel)                   │
│  2. Exchange Hello (version, capabilities)                          │
│  3. If PSK: complete challenge-response auth                        │
│  4. Send TestStart (protocol, streams, duration, direction)         │
│  5. Receive TestAck (data ports)                                    │
│  6. Open data connections (TCP/UDP/QUIC) to data ports              │
│  7. Transfer data, receive Interval messages                        │
│  8. Receive Result message                                          │
│  9. Display results                                                  │
└────────────────────────────────────────────────────────────────────┘

┌────────────────────────────────────────────────────────────────────┐
│ SERVER                                                              │
│                                                                     │
│  1. Accept control connection                                       │
│  2. Send Hello (version, auth challenge if PSK)                     │
│  3. If PSK: verify auth response                                    │
│  4. Receive TestStart                                               │
│  5. Allocate dynamic data ports                                     │
│  6. Send TestAck with data ports                                    │
│  7. Accept data connections                                         │
│  8. Transfer data, send Interval messages                           │
│  9. Send Result message                                             │
│ 10. Clean up, ready for next client                                 │
└────────────────────────────────────────────────────────────────────┘
```

### Control Protocol

The control channel uses newline-delimited JSON messages (one JSON object per line):

```
{"Hello":{"version":"0.3.0","capabilities":[]}}\n
{"TestStart":{"protocol":"tcp","streams":4,...}}\n
```

Each message is serialized as compact JSON followed by a newline character.
The receiver reads line-by-line and deserializes each line as a `ControlMessage`.

Message types (see `protocol.rs`):
- `Hello` - Version and capability exchange
- `AuthResponse` - PSK authentication
- `AuthSuccess` - Auth confirmation
- `TestStart` - Test parameters
- `TestAck` - Data port assignments
- `Interval` - Periodic statistics
- `Result` - Final test result
- `Cancel` / `Cancelled` - Test cancellation
- `Error` - Error messages

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

```rust
// Main accept loop
loop {
    let (stream, addr) = listener.accept().await?;
    tokio::spawn(handle_client(stream, addr, state.clone()));
}

// Per-client task spawns data handlers
async fn handle_client(...) {
    // Control channel handling
    // Spawn data transfer tasks
    let handles: Vec<JoinHandle<_>> = (0..streams)
        .map(|i| tokio::spawn(transfer_data(i, ...)))
        .collect();

    // Aggregate stats from channels
    // Send Interval messages
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
    pub protocol: Protocol,
    pub direction: Direction,
    pub duration_secs: f64,
    pub bytes_transferred: u64,
    pub throughput_mbps: f64,
    pub streams: Vec<StreamResult>,
    pub retransmits: Option<u32>,
    pub jitter_ms: Option<f64>,
    pub lost_packets: Option<u64>,
    pub total_packets: Option<u64>,
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

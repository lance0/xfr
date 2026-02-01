# xfr

A fast, modern network bandwidth testing tool with TUI. Built in Rust as an iperf replacement.

## Install

```bash
cargo install xfr
```

## Usage

### Server

```bash
xfr serve                    # Listen on port 5201
xfr serve -p 9000            # Custom port
xfr serve --one-off          # Exit after one test
```

### Client

```bash
xfr 192.168.1.1              # TCP test, 10s, single stream
xfr 192.168.1.1 -t 30s       # 30 second test
xfr 192.168.1.1 -P 4         # 4 parallel streams
xfr 192.168.1.1 -R           # Reverse (download test)
xfr 192.168.1.1 --bidir      # Bidirectional
```

### UDP Mode

```bash
xfr 192.168.1.1 -u           # UDP mode
xfr 192.168.1.1 -u -b 1G     # UDP at 1 Gbps
```

### Output

```bash
xfr 192.168.1.1 --json       # JSON to stdout
xfr 192.168.1.1 -o out.json  # Save to file
xfr 192.168.1.1 --no-tui     # Plain text, no TUI
```

### Compare Results

```bash
xfr diff baseline.json current.json
xfr diff baseline.json current.json --threshold 5%
```

### Discovery

```bash
xfr discover                 # Find xfr servers on LAN
```

## Features

| Feature | iperf3 | xfr |
|---------|--------|-----|
| Live TUI | No | Yes |
| Multi-client server | No | Yes |
| Output formats | Text/JSON | Text/JSON/Prometheus |
| Compare runs | No | `xfr diff` |
| LAN discovery | No | `xfr discover` |
| Install | Package manager | `cargo install xfr` |

## Flags

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--port` | `-p` | 5201 | Server/client port |
| `--time` | `-t` | 10s | Test duration |
| `--udp` | `-u` | false | UDP mode |
| `--bitrate` | `-b` | unlimited | Target bitrate |
| `--parallel` | `-P` | 1 | Parallel streams |
| `--reverse` | `-R` | false | Reverse direction |
| `--bidir` | | false | Bidirectional |
| `--json` | | false | JSON output |
| `--output` | `-o` | stdout | Output file |
| `--no-tui` | | false | Disable TUI |
| `--tcp-nodelay` | | false | Disable Nagle |
| `--window` | | OS default | TCP window size |

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at your option.

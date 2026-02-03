# Scripting & CI/CD Guide

xfr is designed for automation with non-interactive modes, structured output, and CI-friendly features.

## Non-Interactive Modes

For scripts and CI pipelines, use these flags:

```bash
xfr host --no-tui         # Disable TUI, plain text output
xfr host --json           # JSON summary at end
xfr host --json-stream    # JSON per interval (for real-time parsing)
xfr host --csv            # CSV output
xfr host -q               # Quiet mode (summary only)
```

### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Test completed successfully |
| 1 | Connection or protocol error |
| 2 | Invalid arguments |
| 3 | Server rejected test (auth, rate limit, ACL) |

## JSON Output

### Summary JSON (`--json`)

```bash
xfr host --json --no-tui > result.json
```

```json
{
  "id": "550e8400-e29b-41d4-a716-446655440000",
  "bytes_total": 1234567890,
  "duration_ms": 10000,
  "throughput_mbps": 987.65,
  "streams": [
    {"id": 0, "bytes": 1234567890, "throughput_mbps": 987.65, "retransmits": 42}
  ],
  "tcp_info": {"retransmits": 42, "rtt_us": 1200, "rtt_var_us": 100, "cwnd": 10}
}
```

### Streaming JSON (`--json-stream`)

One JSON object per line, per interval:

```bash
xfr host --json-stream --no-tui
```

```
{"elapsed_ms":1000,"throughput_mbps":950.2,"bytes":118775000}
{"elapsed_ms":2000,"throughput_mbps":980.5,"bytes":122562500}
...
```

### Parsing with jq

```bash
# Get final throughput
xfr host --json --no-tui | jq '.throughput_mbps'

# Get average from streaming output
xfr host --json-stream --no-tui | jq -s 'map(.throughput_mbps) | add / length'

# Check if throughput meets threshold
xfr host --json --no-tui | jq -e '.throughput_mbps > 900' || echo "FAIL"
```

## CSV Output

```bash
xfr host --csv --no-tui > results.csv
```

Output format:
```
timestamp,elapsed_ms,throughput_mbps,bytes,retransmits
2024-01-15T10:30:00Z,1000,950.2,118775000,0
2024-01-15T10:30:01Z,2000,980.5,122562500,2
```

## CI/CD Examples

### GitHub Actions

```yaml
name: Network Performance

on:
  schedule:
    - cron: '0 */4 * * *'  # Every 4 hours

jobs:
  bandwidth-test:
    runs-on: ubuntu-latest
    steps:
      - name: Install xfr
        run: cargo install xfr

      - name: Run bandwidth test
        run: |
          xfr ${{ secrets.TEST_SERVER }} \
            --json --no-tui \
            -t 30s -P 4 \
            > result.json

      - name: Check threshold
        run: |
          THROUGHPUT=$(jq '.throughput_mbps' result.json)
          if (( $(echo "$THROUGHPUT < 100" | bc -l) )); then
            echo "::error::Throughput $THROUGHPUT Mbps below 100 Mbps threshold"
            exit 1
          fi

      - name: Upload results
        uses: actions/upload-artifact@v4
        with:
          name: bandwidth-result
          path: result.json
```

### GitLab CI

```yaml
network-test:
  image: rust:latest
  script:
    - cargo install xfr
    - xfr $TEST_SERVER --json --no-tui -t 30s > result.json
    - |
      THROUGHPUT=$(jq '.throughput_mbps' result.json)
      if [ $(echo "$THROUGHPUT < 100" | bc) -eq 1 ]; then
        echo "Throughput too low: $THROUGHPUT Mbps"
        exit 1
      fi
  artifacts:
    paths:
      - result.json
```

## Docker Usage

xfr doesn't require special capabilities (unlike tools needing raw sockets).

### Run as Server

```bash
docker run --rm -p 5201:5201 rust:slim sh -c \
  'cargo install xfr && xfr serve'
```

### Run as Client

```bash
docker run --rm rust:slim sh -c \
  'cargo install xfr && xfr host.docker.internal --json --no-tui'
```

### Docker Compose

```yaml
version: '3.8'
services:
  xfr-server:
    image: rust:slim
    command: sh -c 'cargo install xfr && xfr serve'
    ports:
      - "5201:5201"
```

## Shell Script Examples

### Baseline Comparison

```bash
#!/bin/bash
# Compare current performance against baseline

BASELINE="baseline.json"
THRESHOLD=5  # Percent regression allowed

# Run test
xfr $SERVER --json --no-tui -t 30s > current.json

# Compare
if [ -f "$BASELINE" ]; then
    xfr diff "$BASELINE" current.json --threshold ${THRESHOLD}
    if [ $? -ne 0 ]; then
        echo "Performance regression detected!"
        exit 1
    fi
fi

# Update baseline on success
cp current.json "$BASELINE"
```

### Multi-Protocol Test Suite

```bash
#!/bin/bash
# Test all protocols and aggregate results

SERVER=$1
RESULTS_DIR="results/$(date +%Y%m%d_%H%M%S)"
mkdir -p "$RESULTS_DIR"

echo "Testing TCP..."
xfr $SERVER --json --no-tui -t 10s > "$RESULTS_DIR/tcp.json"

echo "Testing UDP at 1G..."
xfr $SERVER -u -b 1G --json --no-tui -t 10s > "$RESULTS_DIR/udp.json"

echo "Testing QUIC..."
xfr $SERVER --quic --json --no-tui -t 10s > "$RESULTS_DIR/quic.json"

echo "Testing multi-stream..."
xfr $SERVER -P 4 --json --no-tui -t 10s > "$RESULTS_DIR/multi.json"

# Summary
echo ""
echo "Results:"
for f in "$RESULTS_DIR"/*.json; do
    NAME=$(basename "$f" .json)
    MBPS=$(jq '.throughput_mbps' "$f")
    printf "%-10s %8.2f Mbps\n" "$NAME" "$MBPS"
done
```

### Regression Detection

```bash
#!/bin/bash
# Detect performance regressions in CI

set -e

SERVER=$1
MIN_THROUGHPUT=${2:-100}  # Minimum acceptable Mbps

# Run test
RESULT=$(xfr $SERVER --json --no-tui -t 30s)
THROUGHPUT=$(echo "$RESULT" | jq '.throughput_mbps')
RETRANSMITS=$(echo "$RESULT" | jq '.retransmits // 0')

echo "Throughput: $THROUGHPUT Mbps"
echo "Retransmits: $RETRANSMITS"

# Check throughput
if (( $(echo "$THROUGHPUT < $MIN_THROUGHPUT" | bc -l) )); then
    echo "FAIL: Throughput below ${MIN_THROUGHPUT} Mbps"
    exit 1
fi

# Check retransmit rate (>1% is concerning)
BYTES=$(echo "$RESULT" | jq '.bytes_total')
RETRANSMIT_RATE=$(echo "scale=4; $RETRANSMITS * 1500 * 100 / $BYTES" | bc)
if (( $(echo "$RETRANSMIT_RATE > 1" | bc -l) )); then
    echo "WARN: High retransmit rate: ${RETRANSMIT_RATE}%"
fi

echo "PASS"
```

## Prometheus Integration

### Push Gateway

Push results after each test (useful for CI):

```bash
# Server configured to push on test completion
xfr serve --push-gateway http://pushgateway:9091

# Or push from client-side script
xfr host --json --no-tui | jq -r '
  "xfr_throughput_mbps \(.throughput_mbps)\n" +
  "xfr_bytes_total \(.bytes_total)\n" +
  "xfr_retransmits_total \(.retransmits // 0)"
' | curl --data-binary @- http://pushgateway:9091/metrics/job/xfr/instance/$(hostname)
```

### Metrics Endpoint

For continuous monitoring (requires `--features prometheus`):

```bash
xfr serve --prometheus 9090
```

Metrics available at `http://localhost:9090/metrics`:

```
# HELP xfr_bytes_total Total bytes transferred
# TYPE xfr_bytes_total counter
xfr_bytes_total{direction="upload"} 1234567890

# HELP xfr_throughput_mbps Current throughput
# TYPE xfr_throughput_mbps gauge
xfr_throughput_mbps{direction="upload"} 987.65

# HELP xfr_active_tests Number of active tests
# TYPE xfr_active_tests gauge
xfr_active_tests 2
```

### Grafana Dashboard

See `examples/grafana-dashboard.json` for a pre-built dashboard with:

- Throughput over time graph
- Active tests gauge
- Retransmit rate panel
- Per-client breakdown

## Environment Variables

Configure xfr via environment for containerized deployments:

```bash
export XFR_PORT=9000
export XFR_DURATION=30s
export XFR_LOG_LEVEL=debug
export XFR_LOG_FILE=/var/log/xfr.log
```

## Tips for Automation

1. **Always use `--no-tui`** in scripts - prevents terminal escape codes in output
2. **Use `--json-stream`** for real-time monitoring - parse each line as it arrives
3. **Set explicit durations** with `-t` - don't rely on defaults
4. **Use `xfr diff`** for regression detection - handles threshold comparison
5. **Check exit codes** - non-zero means failure
6. **Log to file** with `--log-file` for debugging - keeps stdout clean for JSON

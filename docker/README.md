# Docker repro harness — issue #70

Local reproduction of the issue-#70 saturation pattern (live UDP loss
counter stuck during upload-mode tests on a constrained link). Use
this to A/B test changes against the released v0.9.13 baseline before
publishing a new build.

## Build

```
docker build -t xfr-repro -f docker/Dockerfile.repro .
```

The build produces a single image with two binaries side-by-side:

- `/usr/local/bin/xfr` — the current branch (built from `src/`)
- `/usr/local/bin/xfr-baseline` — released v0.9.13 from crates.io

## Run

```
docker run --rm --cap-add=NET_ADMIN xfr-repro              # new build
docker run --rm --cap-add=NET_ADMIN xfr-repro --baseline   # v0.9.13
```

`--cap-add=NET_ADMIN` is required so the entrypoint can apply
`tc netem rate 100mbit delay 50ms` on loopback inside the container.
The host is not privileged.

## Recipe

The entrypoint matches the conditions in brettowe's #70 report as
closely as a loopback shaper allows:

| knob               | value                | why                                                  |
| ------------------ | -------------------- | ---------------------------------------------------- |
| shaper rate        | 100 Mbps on lo       | Wi-Fi-uplink-class capacity                          |
| shaper delay       | 50 ms each-way       | realistic ACK turnaround                             |
| target bitrate     | 1 Gbps               | 10× oversubscription, matches brettowe's recipe      |
| direction          | upload (`-u`)        | the asymmetry that exposed the bug                   |
| duration           | 30 s                 | long enough to see live-vs-end-of-test divergence    |

## Assertions

The new-build invocation has a **hard contract**:

- Maximum interval-line bunching ≤ 2 client-side-timestamp collisions.
  More than 2 lines sharing a single timestamp means the TCP control
  channel is bursting stale intervals on unblock — the v0.9.13
  bunching pattern.
- At least one mid-run interval line carries `lost > 0`. Live loss
  must appear during the run, not just be flushed at the end.

The `--baseline` invocation is **diagnostic only**. It prints the
bunching count and time-to-first-nonzero-loss for narrative evidence
in PR descriptions and issue comments but does not fail on a
threshold. Docker host networking and qdisc behavior aren't
deterministic enough to bake into a CI gate; the existing
`test-control-channel-skew.sh` job at 2× oversubscription is the
automated regression coverage.

## Why a separate harness vs CI

The CI job runs at 2× oversubscription on lo with a tight bunching
threshold and is fast and reliable. This harness runs at 10× to match
brettowe's actual recipe and is for human-driven A/B before
publishing a release. Both are useful for different audiences.

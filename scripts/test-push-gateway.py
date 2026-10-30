#!/usr/bin/env python3
"""Smoke client/server pushes against Pushgateway and vmagent -> VictoriaMetrics."""

import json
import math
import os
import re
import socket
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from urllib.parse import urlencode
from urllib.request import urlopen


class MissingMetric(Exception):
    """A required series has not reached storage yet."""


def require_one(rows):
    if not rows:
        raise MissingMetric
    assert len(rows) == 1, rows
    return rows[0]


def wait_for(check, description):
    deadline = time.monotonic() + 15
    while True:
        try:
            if check():
                return
        except MissingMetric:
            pass
        if time.monotonic() >= deadline:
            raise AssertionError(f"timed out waiting for {description}")
        time.sleep(0.1)


def float_matches_storage(actual, expected, victoria):
    # Twelve significant digits permit up to 1e-11 relative truncation error.
    return actual == expected or (
        victoria and math.isclose(actual, expected, rel_tol=1e-11, abs_tol=1e-12)
    )


def read_samples(url, job, victoria):
    if victoria:
        with urlopen(
            url + "/api/v1/export?" + urlencode({"match[]": f'{{job="{job}"}}'}),
            timeout=5,
        ) as response:
            rows = [json.loads(line) for line in response if line.strip()]
        return [
            (row["metric"]["__name__"], row["metric"], row["values"][-1])
            for row in rows
        ]
    with urlopen(url + "/metrics", timeout=5) as response:
        text = response.read().decode()
    rows = []
    for line in text.splitlines():
        match = re.fullmatch(r"(\w+)\{(.*)\} (\S+)", line)
        if match:
            name, labels, value = match.groups()
            labels = dict(re.findall(r'(\w+)="([^"]*)"', labels))
            if labels.get("job") == job:
                rows.append((name, labels, float(value)))
    return rows


def delivery_matches(samples, result, port, victoria, flags):
    def metric(name, role, stream_id=None, direction=None, view=None):
        return [
            (labels, value)
            for metric_name, labels, value in samples
            if metric_name == name
            and labels.get("role") == role
            and (stream_id is None or labels.get("stream_id") == str(stream_id))
            and (direction is None or labels.get("direction") == direction)
            and (view is None or labels.get("view") == view)
        ]

    def required(*args, **kwargs):
        return require_one(metric(*args, **kwargs))

    client = required("xfr_bytes_total", "client")
    server_row = required("xfr_bytes_total", "server")
    assert client[0]["server"] == f"127.0.0.1:{port}", client
    assert client[1] == result["bytes_total"], (client, result)
    assert server_row[0]["client"] == "127.0.0.1", server_row
    assert server_row[1] > 0, server_row
    if victoria:
        for _, labels, _ in samples:
            role = labels.get("role")
            if role in ("client", "server"):
                assert labels.get("host") == f"smoke-{role}", labels
    for name, expected in [
        ("xfr_throughput_mbps", result["throughput_mbps"]),
        ("xfr_duration_seconds", result["duration_ms"] / 1000),
    ]:
        actual = required(name, "client")[1]
        assert float_matches_storage(actual, expected, victoria), (
            name,
            actual,
            expected,
        )
    for stream in result["streams"]:
        assert (
            required("xfr_stream_bytes_total", "client", stream["id"])[1]
            == stream["bytes"]
        )
    if result.get("udp_stats"):
        assert (
            required("xfr_udp_packets_lost", "client")[1] == result["udp_stats"]["lost"]
        )
    if "-u" in flags:
        if "--bidir" in flags:
            directions = [
                (
                    "upload",
                    result["bytes_sent"],
                    result["throughput_send_mbps"],
                    result.get("udp_stats_send"),
                ),
                (
                    "download",
                    result["bytes_received"],
                    result["throughput_recv_mbps"],
                    result.get("udp_stats_recv"),
                ),
            ]
        else:
            directions = [
                (
                    "download" if "-R" in flags else "upload",
                    result["bytes_total"],
                    result["throughput_mbps"],
                    result.get("udp_stats"),
                )
            ]
        for direction, expected_bytes, expected_rate, quality in directions:
            assert (
                required(
                    "xfr_udp_direction_bytes",
                    "client",
                    direction=direction,
                    view="receiver",
                )[1]
                == expected_bytes
            )
            actual_rate = required(
                "xfr_udp_direction_throughput_mbps",
                "client",
                direction=direction,
                view="receiver",
            )[1]
            assert float_matches_storage(actual_rate, expected_rate, victoria)
            server_view = "receiver" if direction == "upload" else "sender"
            required(
                "xfr_udp_direction_bytes",
                "server",
                direction=direction,
                view=server_view,
            )
            if quality and (quality["packets_received"] > 0 or quality["lost"] > 0):
                for suffix, field in [
                    ("packets_expected", "packets_sent"),
                    ("packets_received", "packets_received"),
                    ("packets_lost", "lost"),
                    ("lost_percent", "lost_percent"),
                ]:
                    actual = required(
                        "xfr_udp_direction_" + suffix,
                        "client",
                        direction=direction,
                    )[1]
                    assert float_matches_storage(actual, quality[field], victoria), (
                        suffix,
                        direction,
                        actual,
                        quality,
                    )
                if quality["packets_received"] > 0:
                    actual = required(
                        "xfr_udp_direction_jitter_ms",
                        "client",
                        direction=direction,
                    )[1]
                    assert float_matches_storage(actual, quality["jitter_ms"], victoria)
            else:
                assert not metric(
                    "xfr_udp_direction_packets_lost",
                    "client",
                    direction=direction,
                )
        assert not metric(
            "xfr_udp_direction_packets_lost", "server", direction="download"
        )
    else:
        assert not any(name.startswith("xfr_udp_direction_") for name, _, _ in samples)
    return True


def run(binary, gateway, storage, victoria, flags):
    client_gateway = server_gateway = gateway
    if victoria:
        separator = "&" if "?" in gateway else "?"
        client_gateway += separator + urlencode({"extra_label": "host=smoke-client"})
        server_gateway += separator + urlencode({"extra_label": "host=smoke-server"})
    with socket.socket() as sock:
        sock.bind(("127.0.0.1", 0))
        port = sock.getsockname()[1]
    with (
        tempfile.TemporaryDirectory() as directory,
        tempfile.TemporaryFile(mode="w+") as log,
    ):
        env = dict(os.environ, XDG_CONFIG_HOME=directory)
        env.pop("XFR_PUSH_GATEWAY", None)
        server = subprocess.Popen(
            [
                binary,
                "serve",
                "-p",
                str(port),
                "-4",
                "--no-mdns",
                "--one-off",
                "--push-gateway",
                server_gateway,
            ],
            stdout=log,
            stderr=subprocess.STDOUT,
            env=env,
        )
        try:

            def listening():
                assert server.poll() is None, "xfr server exited during startup"
                try:
                    with socket.create_connection(("127.0.0.1", port), timeout=0.1):
                        return True
                except OSError:
                    return False

            wait_for(listening, "xfr listener")
            output = subprocess.run(
                [
                    binary,
                    "127.0.0.1",
                    "-p",
                    str(port),
                    "-P",
                    "2",
                    "-b",
                    "2M",
                    "-t",
                    "1",
                    "--json",
                    "--no-update-check",
                    "--push-gateway",
                    client_gateway,
                    *flags,
                ],
                capture_output=True,
                text=True,
                timeout=20,
                env=env,
                check=True,
            )
            result = json.loads(output.stdout)
            assert result["bytes_total"] > 0, result
            assert server.wait(timeout=15) == 0, "xfr server failed"
            log.seek(0)
            assert "Failed to push" not in log.read() + output.stderr
            job = "xfr_test_" + result["id"]

            def delivered():
                return delivery_matches(
                    read_samples(storage, job, victoria), result, port, victoria, flags
                )

            wait_for(delivered, f"both roles in {storage}")
            print(
                f"PASS {'vmagent -> VictoriaMetrics' if victoria else 'Pushgateway'} {flags or ['TCP']} {result['id']}"
            )
        except Exception:
            log.seek(0)
            print(log.read(), file=sys.stderr)
            raise
        finally:
            if server.poll() is None:
                server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait()


def self_check_delivery():
    result = {
        "bytes_total": 8,
        "throughput_mbps": 0.000064,
        "duration_ms": 1000,
        "streams": [],
    }
    client = {"role": "client", "server": "127.0.0.1:5201"}
    server = {"role": "server", "client": "127.0.0.1"}
    legacy = [
        ("xfr_bytes_total", client, 8),
        ("xfr_bytes_total", server, 8),
        ("xfr_throughput_mbps", client, result["throughput_mbps"]),
        ("xfr_duration_seconds", client, 1),
    ]
    client_direction = dict(client, direction="download", view="receiver")
    client_rows = [
        ("xfr_udp_direction_bytes", client_direction, 8),
        (
            "xfr_udp_direction_throughput_mbps",
            client_direction,
            result["throughput_mbps"],
        ),
    ]
    complete = (
        legacy
        + client_rows
        + [
            (
                "xfr_udp_direction_bytes",
                dict(server, direction="download", view="sender"),
                8,
            )
        ]
    )
    snapshots = [legacy, legacy + client_rows, complete]
    calls = 0

    def delayed():
        nonlocal calls
        snapshot = snapshots[calls]
        calls += 1
        return delivery_matches(snapshot, result, 5201, False, ["-u", "-R"])

    wait_for(delayed, "delayed directional sample self-check")
    assert calls == 3
    wrong_value = [("xfr_bytes_total", client, 9), *complete[1:]]
    duplicate = [*complete, complete[-1]]
    for invalid in (wrong_value, duplicate):
        attempts = 0

        def invalid_snapshot(snapshot=invalid):
            nonlocal attempts
            attempts += 1
            return delivery_matches(snapshot, result, 5201, False, ["-u", "-R"])

        try:
            wait_for(invalid_snapshot, "invalid sample self-check")
        except AssertionError:
            assert attempts == 1, (
                "wrong values and duplicate rows must fail immediately"
            )
        else:
            raise AssertionError("invalid samples must fail without retrying")

    def unrelated_error():
        raise ValueError("unrelated errors must not be retried")

    try:
        wait_for(unrelated_error, "unrelated error self-check")
    except ValueError:
        pass
    else:
        raise AssertionError("unrelated errors must remain fatal")


if __name__ == "__main__":
    self_check_delivery()
    expected = 1.9975544455544458
    assert float_matches_storage(1.99755444555, expected, True)
    assert float_matches_storage(1.0, 1.000000000009, True)
    assert not float_matches_storage(1.99755444555, expected, False)
    assert not float_matches_storage(1.9975544455, expected, True)
    binary, pushgateway, victoria, vmagent = sys.argv[1:]
    binary = str(Path(binary).resolve())
    for gateway, storage, is_victoria in [
        (pushgateway, pushgateway, False),
        (vmagent, victoria, True),
    ]:
        for flags in [[], ["-u", "-R"], ["-u", "--bidir"], ["-Q"]]:
            run(binary, gateway, storage, is_victoria, flags)

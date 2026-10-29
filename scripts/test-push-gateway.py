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


def wait_for(check, description):
    deadline = time.monotonic() + 15
    while True:
        if check():
            return
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
                samples = read_samples(storage, job, victoria)

                def metric(name, role, stream_id=None):
                    return [
                        (labels, value)
                        for metric_name, labels, value in samples
                        if metric_name == name
                        and labels.get("role") == role
                        and (
                            stream_id is None
                            or labels.get("stream_id") == str(stream_id)
                        )
                    ]

                client = metric("xfr_bytes_total", "client")
                server_rows = metric("xfr_bytes_total", "server")
                if not client or not server_rows:
                    return False
                assert len(client) == len(server_rows) == 1, samples
                assert client[0][0]["server"] == f"127.0.0.1:{port}", client
                assert client[0][1] == result["bytes_total"], (client, result)
                assert server_rows[0][0]["client"] == "127.0.0.1", server_rows
                assert server_rows[0][1] > 0, server_rows
                if victoria:
                    for _, labels, _ in samples:
                        role = labels.get("role")
                        if role in ("client", "server"):
                            assert labels.get("host") == f"smoke-{role}", labels
                for name, expected in [
                    ("xfr_throughput_mbps", result["throughput_mbps"]),
                    ("xfr_duration_seconds", result["duration_ms"] / 1000),
                ]:
                    actual = metric(name, "client")[0][1]
                    assert float_matches_storage(actual, expected, victoria), (
                        name,
                        actual,
                        expected,
                    )
                for stream in result["streams"]:
                    assert (
                        metric("xfr_stream_bytes_total", "client", stream["id"])[0][1]
                        == stream["bytes"]
                    )
                if result.get("udp_stats"):
                    assert (
                        metric("xfr_udp_packets_lost", "client")[0][1]
                        == result["udp_stats"]["lost"]
                    )
                return True

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


if __name__ == "__main__":
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

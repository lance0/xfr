//! Throughput benchmarks

use std::time::{Duration, Instant};

use std::hint::black_box;

use criterion::{Criterion, criterion_group, criterion_main};

use xfr::stats::StreamStats;
use xfr::udp::{JitterCalculator, UdpPacketHeader};

fn bench_stats_add_bytes(c: &mut Criterion) {
    let stats = StreamStats::new(0);

    c.bench_function("stats_add_bytes", |b| {
        b.iter(|| {
            stats.add_bytes_sent(black_box(1400));
        })
    });
}

fn bench_udp_header_encode(c: &mut Criterion) {
    let header = UdpPacketHeader {
        sequence: 12345,
        timestamp_us: 67890,
    };
    let mut buffer = [0u8; 16];

    c.bench_function("udp_header_encode", |b| {
        b.iter(|| {
            header.encode(black_box(&mut buffer));
        })
    });
}

fn bench_udp_header_decode(c: &mut Criterion) {
    let mut buffer = [0u8; 16];
    let header = UdpPacketHeader {
        sequence: 12345,
        timestamp_us: 67890,
    };
    header.encode(&mut buffer);

    c.bench_function("udp_header_decode", |b| {
        b.iter(|| UdpPacketHeader::decode(black_box(&buffer)))
    });
}

fn bench_interval_record(c: &mut Criterion) {
    let stats = StreamStats::new(0);
    // Simulate some data transfer
    for _ in 0..1000 {
        stats.add_bytes_sent(1400);
    }

    c.bench_function("interval_record", |b| {
        b.iter(|| {
            black_box(stats.record_interval());
        })
    });
}

fn bench_jitter_calculation(c: &mut Criterion) {
    let start = Instant::now();

    c.bench_function("jitter_calculation", |b| {
        let mut calc = JitterCalculator::new();
        let mut seq = 0u64;
        b.iter(|| {
            let recv_time = start + Duration::from_micros(seq * 1000);
            black_box(calc.update(black_box(seq * 1000), recv_time));
            seq += 1;
        })
    });
}

fn bench_tcp_send_loop_overhead(c: &mut Criterion) {
    // Benchmark the non-I/O overhead of the TCP send loop:
    // buffer access + stats update
    let stats = StreamStats::new(0);
    let buffer = vec![0u8; 131072]; // 128KB buffer

    c.bench_function("tcp_send_loop_overhead", |b| {
        b.iter(|| {
            // Simulate what happens per write (minus actual I/O)
            let _ = black_box(&buffer[..]);
            stats.add_bytes_sent(black_box(buffer.len() as u64));
        })
    });
}

criterion_group!(
    benches,
    bench_stats_add_bytes,
    bench_udp_header_encode,
    bench_udp_header_decode,
    bench_interval_record,
    bench_jitter_calculation,
    bench_tcp_send_loop_overhead
);
criterion_main!(benches);

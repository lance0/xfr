//! Throughput benchmarks

use criterion::{Criterion, black_box, criterion_group, criterion_main};

use xfr::stats::StreamStats;
use xfr::udp::UdpPacketHeader;

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

criterion_group!(
    benches,
    bench_stats_add_bytes,
    bench_udp_header_encode,
    bench_udp_header_decode
);
criterion_main!(benches);

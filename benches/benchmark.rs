use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use ttyman::terminal::Terminal;

fn bench_vt100_parser_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("vt100_parser");

    // Prepare 10,000 lines of typical ANSI terminal output
    let mut sample_ansi = Vec::new();
    for i in 0..10_000 {
        sample_ansi.extend_from_slice(
            format!("\x1b[32m[INFO]\x1b[0m \x1b[1mTask #{i}\x1b[0m completed in \x1b[33m0.042s\x1b[0m with code 0\r\n").as_bytes(),
        );
    }
    let data_len = sample_ansi.len() as u64;
    group.throughput(Throughput::Bytes(data_len));

    group.bench_function("process_ansi_10k_lines", |b| {
        b.iter_batched(
            || Terminal::new(24, 80, 10_000),
            |terminal| {
                terminal.process(&sample_ansi);
            },
            criterion::BatchSize::SmallInput,
        )
    });

    group.finish();
}

criterion_group!(benches, bench_vt100_parser_throughput);
criterion_main!(benches);

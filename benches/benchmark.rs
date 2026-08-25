use criterion::{BenchmarkId, Criterion, Throughput, criterion_group, criterion_main};
use nix::pty::openpty;
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::Duration;
use ttyman::terminal::Terminal;

/// Measure execution of a command inside an isolated PTY, reading all output to EOF
fn run_command_in_pty(cmd: &[&str]) -> usize {
    let pty_res = openpty(None, None).expect("failed to openpty");
    let mut master_file = std::fs::File::from(pty_res.master);
    let slave_stdin = std::fs::File::from(pty_res.slave);
    let slave_stdout = slave_stdin.try_clone().expect("failed to clone slave fd");
    let slave_stderr = slave_stdin.try_clone().expect("failed to clone slave fd");

    let mut child = Command::new(cmd[0])
        .args(&cmd[1..])
        .stdin(Stdio::from(slave_stdin))
        .stdout(Stdio::from(slave_stdout))
        .stderr(Stdio::from(slave_stderr))
        .spawn()
        .expect("failed to spawn command");

    let mut total_bytes = 0;
    let mut buf = [0u8; 65536];

    loop {
        match master_file.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => total_bytes += n,
            Err(_) => break,
        }
    }

    let _ = child.wait();
    total_bytes
}

fn bench_vt100_parser_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("vt100_parser");

    // Prepare 10,000 lines of typical ANSI terminal output
    let mut sample_ansi = Vec::new();
    for i in 0..10_000 {
        sample_ansi.extend_from_slice(
            format!("\x1b[32m[INFO]\x1b[0m \x1b[1mTask #{i}\x1b[0m completed in \x1b[33m0.042s\x1b[0m with code 0\r\n").as_bytes()
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

fn bench_pty_throughput(c: &mut Criterion) {
    let mut group = c.benchmark_group("pty_throughput");
    group.sample_size(10);
    group.warm_up_time(Duration::from_secs(1));
    group.measurement_time(Duration::from_secs(4));

    let lines = "100000"; // 100,000 lines for fast statistical sampling
    let ttyman_bin = env!("CARGO_BIN_EXE_ttyman");

    group.bench_with_input(BenchmarkId::new("mode", "direct_pty"), &lines, |b, &l| {
        b.iter(|| {
            run_command_in_pty(&["seq", "1", l]);
        });
    });

    group.bench_with_input(BenchmarkId::new("mode", "ttyman_pty"), &lines, |b, &l| {
        b.iter(|| {
            run_command_in_pty(&[ttyman_bin, "run", "--", "seq", "1", l]);
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_vt100_parser_throughput,
    bench_pty_throughput
);
criterion_main!(benches);

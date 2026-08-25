use crate::{FrameReader, read_header};
use clap::Args;
use nix::libc;
use nix::sys::stat::stat;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::time::{Duration, Instant};

#[derive(Args, Debug, Clone)]
pub struct PlayArgs {
    /// Playback speed multiplier (e.g. 1.0, 2.0, 0.5)
    #[arg(short = 's', long = "speed", default_value_t = 1.0)]
    pub speed: f64,

    /// No-wait mode (play immediately without delay)
    #[arg(short = 'n', long = "no-wait")]
    pub no_wait: bool,

    /// Follow mode (tail growing file)
    #[arg(short = 'f', long = "follow")]
    pub follow: bool,

    /// Inspect and print total duration of recorded session(s) without playing
    #[arg(short = 't', long = "time")]
    pub time: bool,

    /// Files to play or inspect (reads from stdin if omitted or '-')
    pub files: Vec<String>,
}

fn calculate_frame_micros(prev_sec: u64, prev_usec: u32, cur_sec: u64, cur_usec: u32) -> i64 {
    let diff_sec = (cur_sec as i64) - (prev_sec as i64);
    let diff_usec = (cur_usec as i64) - (prev_usec as i64);
    diff_sec * 1_000_000 + diff_usec
}

fn calc_time_seekable<R: Read + Seek>(mut reader: R) -> io::Result<u64> {
    let start = match read_header(&mut reader) {
        Ok(Some(h)) => h,
        _ => return Ok(0),
    };

    let mut end = start;
    if reader.seek(SeekFrom::Current(start.len as i64)).is_err() {
        return Ok(0);
    }

    while let Ok(Some(h)) = read_header(&mut reader) {
        end = h;
        if reader.seek(SeekFrom::Current(h.len as i64)).is_err() {
            break;
        }
    }

    Ok(end.sec.saturating_sub(start.sec))
}

fn calc_time_streaming<R: Read>(mut reader: R) -> io::Result<u64> {
    let start = match read_header(&mut reader) {
        Ok(Some(h)) => h,
        _ => return Ok(0),
    };

    let mut end = start;
    if io::copy(&mut (&mut reader).take(start.len as u64), &mut io::sink()).is_err() {
        return Ok(0);
    }

    while let Ok(Some(h)) = read_header(&mut reader) {
        end = h;
        if io::copy(&mut (&mut reader).take(h.len as u64), &mut io::sink()).is_err() {
            break;
        }
    }

    Ok(end.sec.saturating_sub(start.sec))
}

pub fn calc_time(path_str: &str) -> u64 {
    if path_str == "-" {
        let stdin = io::stdin();
        calc_time_streaming(BufReader::new(stdin)).unwrap_or(0)
    } else {
        let path = Path::new(path_str);
        let file = match File::open(path) {
            Ok(f) => f,
            Err(_) => return 0,
        };
        let buf = BufReader::new(file);
        calc_time_seekable(buf).unwrap_or(0)
    }
}

pub fn run(args: PlayArgs) -> anyhow::Result<()> {
    if args.time {
        let targets = if args.files.is_empty() {
            vec!["-".to_string()]
        } else {
            args.files
        };
        for file in targets {
            let duration = calc_time(&file);
            println!("{:7}\t{}", duration, file);
        }
        return Ok(());
    }

    if args.speed <= 0.0 {
        eprintln!("-s option requires a strictly positive number");
        std::process::exit(1);
    }

    let target_file = args.files.first().map(|s| s.as_str()).unwrap_or("-");
    let is_regular_file = if target_file != "-" {
        if let Ok(st) = stat(Path::new(target_file)) {
            (st.st_mode & libc::S_IFMT) == libc::S_IFREG
        } else {
            false
        }
    } else {
        false
    };

    let input_reader: Box<dyn Read> = if target_file != "-" {
        let file = File::open(Path::new(target_file))?;
        Box::new(file)
    } else {
        Box::new(io::stdin())
    };

    let mut stdout = io::stdout();
    let mut frame_reader = FrameReader::new(input_reader);
    let mut drift = Duration::ZERO;
    let mut prev_time: Option<(u64, u32)> = None;
    let mut last_emit_instant: Option<Instant> = None;

    loop {
        match frame_reader.read_next_frame() {
            Ok(Some(frame)) => {
                if !args.no_wait
                    && let Some((prev_sec, prev_usec)) = prev_time
                {
                    let diff_micros = calculate_frame_micros(
                        prev_sec,
                        prev_usec,
                        frame.header.sec,
                        frame.header.usec,
                    );
                    if diff_micros > 0 {
                        let scaled_micros = ((diff_micros as f64) / args.speed) as i64;
                        let target_duration = Duration::from_micros(scaled_micros.max(0) as u64);
                        let elapsed = last_emit_instant
                            .map(|i| i.elapsed())
                            .unwrap_or(Duration::ZERO);

                        let remaining_target = if target_duration > elapsed {
                            target_duration - elapsed
                        } else {
                            Duration::ZERO
                        };

                        let effective_duration = if remaining_target > drift {
                            remaining_target - drift
                        } else {
                            Duration::ZERO
                        };

                        if effective_duration > Duration::ZERO {
                            let start_sleep = Instant::now();
                            std::thread::sleep(effective_duration);
                            let sleep_elapsed = start_sleep.elapsed();
                            if sleep_elapsed > effective_duration {
                                drift = sleep_elapsed - effective_duration;
                            } else {
                                drift = Duration::ZERO;
                            }
                        }
                    }
                }

                prev_time = Some((frame.header.sec, frame.header.usec));
                last_emit_instant = Some(Instant::now());
                stdout.write_all(&frame.data)?;
                stdout.flush()?;
            }
            Ok(None) => {
                if args.follow && is_regular_file {
                    std::thread::sleep(Duration::from_millis(100));
                    continue;
                } else {
                    break;
                }
            }
            Err(e) => {
                eprintln!("ttyman play error: {e}");
                break;
            }
        }
    }

    Ok(())
}

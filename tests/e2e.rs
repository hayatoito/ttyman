use std::io::Write;
use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_record_play_time_flow() {
    let dir = tempdir().unwrap();
    let rec_path = dir.path().join("session.ttyrec");
    let bin = env!("CARGO_BIN_EXE_ttyman");

    // 1. Record stream using `ttyproxy record <FILE>`
    let mut record_child = Command::new(bin)
        .arg("record")
        .arg(&rec_path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn ttyproxy record");

    if let Some(mut stdin) = record_child.stdin.take() {
        stdin.write_all(b"hello from ttyproxy record\n").unwrap();
    }
    let status = record_child.wait().unwrap();
    assert!(status.success());
    assert!(rec_path.exists());

    // 2. Inspect duration using `ttyproxy play --time`
    let time_output = Command::new(bin)
        .arg("play")
        .arg("--time")
        .arg(&rec_path)
        .output()
        .expect("failed to run ttyproxy play --time");

    assert!(time_output.status.success());
    let time_str = String::from_utf8_lossy(&time_output.stdout);
    assert!(time_str.contains("session.ttyrec"));

    // 3. Playback using `ttyproxy play -n`
    let play_output = Command::new(bin)
        .arg("play")
        .arg("-n")
        .arg(&rec_path)
        .output()
        .expect("failed to run ttyproxy play");

    assert!(play_output.status.success());
    let play_str = String::from_utf8_lossy(&play_output.stdout);
    assert!(play_str.contains("hello from ttyproxy record"));
}

#[test]
fn test_time_and_play_stdin_pipeline() {
    let dir = tempdir().unwrap();
    let rec_path = dir.path().join("pipeline.ttyrec");
    let bin = env!("CARGO_BIN_EXE_ttyman");

    // Record stream
    let mut child = Command::new(bin)
        .arg("record")
        .arg(&rec_path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn ttyproxy record");

    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(b"pipeline text stream\n").unwrap();
    }
    assert!(child.wait().unwrap().success());

    // Read session file and pipe into `ttyproxy play --time -`
    let file_content = std::fs::read(&rec_path).unwrap();
    let mut time_child = Command::new(bin)
        .arg("play")
        .arg("--time")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn ttyproxy play --time");

    time_child
        .stdin
        .take()
        .unwrap()
        .write_all(&file_content)
        .unwrap();
    let time_out = time_child.wait_with_output().unwrap();
    assert!(time_out.status.success());

    // Read session file and pipe into `ttyproxy play -n -`
    let mut play_child = Command::new(bin)
        .arg("play")
        .arg("-n")
        .arg("-")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn ttyproxy play");

    play_child
        .stdin
        .take()
        .unwrap()
        .write_all(&file_content)
        .unwrap();
    let play_out = play_child.wait_with_output().unwrap();
    assert!(play_out.status.success());
    let play_str = String::from_utf8_lossy(&play_out.stdout);
    assert!(play_str.contains("pipeline text stream"));
}

#[test]
fn test_read_outside_session() {
    let bin = env!("CARGO_BIN_EXE_ttyman");

    let output = Command::new(bin)
        .env_remove("TTYMAN_SESSION")
        .env_remove("TTYMAN_PID")
        .arg("read")
        .output()
        .expect("failed to run ttyman read");

    assert!(!output.status.success());
    let err_str = String::from_utf8_lossy(&output.stderr);
    assert!(err_str.contains("Not running inside a 'ttyman' session"));
}

#[test]
fn test_read_inside_session() {
    let dir = tempdir().unwrap();
    let capture_out_path = dir.path().join("captured.txt");
    let bin = env!("CARGO_BIN_EXE_ttyman");

    // Run script inside ttyproxy run that outputs text, sleeps slightly for pipe flush, calls ttyproxy read
    let script = format!(
        "echo 'HEADER_LINE_1'; echo 'DATA_LINE_2'; sleep 0.1; '{}' read > '{}'",
        bin,
        capture_out_path.display()
    );

    let start_status = Command::new(bin)
        .arg("run")
        .arg("-T")
        .arg("never")
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg(&script)
        .status()
        .expect("failed to run ttyproxy run");

    assert!(start_status.success());
    assert!(capture_out_path.exists());

    let captured_content = std::fs::read_to_string(&capture_out_path).unwrap();
    assert!(captured_content.contains("HEADER_LINE_1"));
    assert!(captured_content.contains("DATA_LINE_2"));
}

#[test]
fn test_write_and_read_e2e() {
    let dir = tempdir().unwrap();
    let out_file = dir.path().join("injected_out.txt");
    let bin = env!("CARGO_BIN_EXE_ttyman");

    // Spawn ttyproxy run running an interactive shell
    let mut session = Command::new(bin)
        .arg("run")
        .arg("-T")
        .arg("always")
        .arg("--")
        .arg("sh")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("failed to start ttyproxy session");

    let pid = session.id();
    let sock_path = ttyman::ipc::default_socket_path(pid).unwrap();

    // Wait slightly for socket creation
    for _ in 0..50 {
        if sock_path.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(sock_path.exists(), "IPC socket was not created in time");

    // Use `ttyproxy write -s <PID> -E` to inject a command that writes to out_file
    let send_status = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(pid.to_string())
        .arg("-E")
        .arg(format!(
            "echo 'INJECTED_CMD_SUCCESS' > '{}'",
            out_file.display()
        ))
        .status()
        .expect("failed to run ttyproxy write");
    assert!(send_status.success());

    // Give child shell time to execute the command
    for _ in 0..50 {
        if out_file.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(out_file.exists());
    let file_content = std::fs::read_to_string(&out_file).unwrap();
    assert!(file_content.contains("INJECTED_CMD_SUCCESS"));

    // Close session by sending exit
    let _ = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(pid.to_string())
        .arg("-E")
        .arg("exit")
        .status();

    let _ = session.wait();
}

#[test]
fn test_remap_translation_e2e() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    let out_bin_path = dir.path().join("received.bin");
    let bin = env!("CARGO_BIN_EXE_ttyman");

    // 1. Write remapping configuration using [u8] byte array literals
    let config_content = r#"
[[remap]]
from = [0x02]
to   = [0x1b, 0x5b, 0x44]

[[remap]]
from = [0x06]
to   = [0x1b, 0x5b, 0x43]

[[remap]]
from = [0x07]
to   = [0x53, 0x54, 0x41, 0x54, 0x55, 0x53, 0x0a]
"#;
    std::fs::write(&config_path, config_content).unwrap();

    // 2. Target child program: Python script that reads exactly 15 bytes and dumps to out_bin_path
    let script = format!(
        "python3 -c \"import sys; data = sys.stdin.buffer.read(15); open('{}', 'wb').write(data)\"",
        out_bin_path.display()
    );

    let mut start_child = Command::new(bin)
        .arg("run")
        .arg("-T")
        .arg("always")
        .arg("--config")
        .arg(&config_path)
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg(&script)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn ttyproxy run with input remapping");

    // 3. Send Ctrl-b (0x02), Ctrl-f (0x06), Ctrl-g (0x07), 'A', '\n'
    if let Some(mut stdin) = start_child.stdin.take() {
        stdin.write_all(&[0x02, 0x06, 0x07, b'A', b'\n']).unwrap();
        stdin.flush().unwrap();
        drop(stdin);
    }

    let out = start_child.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(out_bin_path.exists());

    // 4. Verify received payload
    // Expected: \x1b[D (Left, 3B) + \x1b[C (Right, 3B) + STATUS\n (7B) + A\n (2B) = 15B
    let received_bytes = std::fs::read(&out_bin_path).unwrap();
    let expected = b"\x1b[D\x1b[CSTATUS\nA\n";
    assert_eq!(received_bytes, expected);
}

#[test]
fn test_list_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");

    let mut session = Command::new(bin)
        .arg("run")
        .arg("-T")
        .arg("always")
        .arg("--")
        .arg("sh")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("failed to start session");

    let pid = session.id();
    let sock = ttyman::ipc::default_socket_path(pid).unwrap();

    for _ in 0..50 {
        if sock.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(sock.exists());

    // 1. Check table output
    let list_out = Command::new(bin)
        .arg("list")
        .output()
        .expect("failed to run ttyproxy list");
    assert!(list_out.status.success());
    let list_str = String::from_utf8_lossy(&list_out.stdout);
    assert!(list_str.contains(&pid.to_string()));

    // 2. Check JSON output
    let json_out = Command::new(bin)
        .arg("list")
        .arg("--json")
        .output()
        .expect("failed to run ttyproxy list --json");
    assert!(json_out.status.success());
    let json_str = String::from_utf8_lossy(&json_out.stdout);
    assert!(json_str.contains(&format!("\"pid\": {pid}")));
    assert!(json_str.contains("\"persist\": false"));
    assert!(json_str.contains("\"clients\": 1"));

    // Terminate session
    let _ = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(pid.to_string())
        .arg("-E")
        .arg("exit")
        .status();

    let _ = session.wait();
}

#[test]
fn test_attach_remap_translation_e2e() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config.toml");
    let out_bin_path = dir.path().join("received_attach.bin");
    let bin = env!("CARGO_BIN_EXE_ttyman");
    let session_name = format!("t_att_remap_{}", std::process::id() % 10000);

    // 1. Config with input remapping
    std::fs::write(
        &config_path,
        r#"
[[remap]]
from = [0x02]
to   = [0x1b, 0x5b, 0x44]

[[remap]]
from = [0x06]
to   = [0x1b, 0x5b, 0x43]

[[remap]]
from = [0x07]
to   = [0x53, 0x54, 0x41, 0x54, 0x55, 0x53, 0x0a]
"#,
    )
    .unwrap();

    // 2. Spawn headless background session running python script
    let script = format!(
        "python3 -c \"import sys; data = sys.stdin.buffer.read(15); open('{}', 'wb').write(data)\"",
        out_bin_path.display()
    );

    let spawn_out = Command::new(bin)
        .env("XDG_RUNTIME_DIR", dir.path())
        .arg("start")
        .arg("-s")
        .arg(&session_name)
        .arg("--")
        .arg("sh")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("failed to spawn headless session");
    assert!(spawn_out.status.success());

    std::thread::sleep(std::time::Duration::from_millis(200));

    // 3. Attach to session with -c config_path
    let mut attach_child = Command::new(bin)
        .env("XDG_RUNTIME_DIR", dir.path())
        .arg("attach")
        .arg("-c")
        .arg(&config_path)
        .arg("-s")
        .arg(&session_name)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn attach child");

    std::thread::sleep(std::time::Duration::from_millis(200));

    // 4. Send Ctrl-b (0x02), Ctrl-f (0x06), Ctrl-g (0x07), 'A', '\n'
    if let Some(mut stdin) = attach_child.stdin.take() {
        use std::io::Write;
        stdin.write_all(&[0x02, 0x06, 0x07, b'A', b'\n']).unwrap();
        stdin.flush().unwrap();
        drop(stdin);
    }

    let _ = attach_child.wait();
    std::thread::sleep(std::time::Duration::from_millis(300));

    // 5. Verify received payload: \x1b[D + \x1b[C + STATUS\n + A\n = 15B
    assert!(out_bin_path.exists());
    let received_bytes = std::fs::read(&out_bin_path).unwrap();
    let expected = b"\x1b[D\x1b[CSTATUS\nA\n";
    assert_eq!(received_bytes, expected);

    // Clean up
    let _ = Command::new(bin).arg("list").arg("--clean").output();
}

#[test]
fn test_play_follow_fifo_e2e() {
    use nix::sys::stat::Mode;
    use nix::unistd::mkfifo;

    let dir = tempdir().unwrap();
    let pipe_path = dir.path().join("live.pipe");
    let bin = env!("CARGO_BIN_EXE_ttyman");

    mkfifo(&pipe_path, Mode::from_bits(0o600).unwrap()).expect("failed to create fifo");

    // Spawn player in background reading from FIFO
    let player_child = Command::new(bin)
        .arg("play")
        .arg(&pipe_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn ttyproxy play");

    // Spawn recorder writing into FIFO
    let mut recorder_child = Command::new(bin)
        .arg("record")
        .arg(&pipe_path)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn ttyproxy record");

    // Write text chunks into recorder
    if let Some(mut stdin) = recorder_child.stdin.take() {
        stdin.write_all(b"LIVE STREAM BROADCAST TEST\n").unwrap();
        stdin.flush().unwrap();
    }
    assert!(recorder_child.wait().unwrap().success());

    // When recorder closes the FIFO, player should finish gracefully on EOF
    let player_out = player_child.wait_with_output().unwrap();
    assert!(player_out.status.success());
    let play_str = String::from_utf8_lossy(&player_out.stdout);
    assert!(
        play_str.contains("LIVE STREAM BROADCAST TEST"),
        "expected broadcasted text in output, got: {play_str}"
    );
}

#[test]
fn test_play_regular_file_ctrl_c_quit_e2e() {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    let dir = tempdir().unwrap();
    let file_path = dir.path().join("delayed.ttyrec");
    let bin = env!("CARGO_BIN_EXE_ttyman");

    let header1 = ttyman::Header::new(1000, 0, 8).unwrap();
    let frame1 = ttyman::Frame {
        header: header1,
        data: b"FRAME 1\n".to_vec(),
    };
    let header2 = ttyman::Header::new(1010, 0, 8).unwrap(); // 10s later
    let frame2 = ttyman::Frame {
        header: header2,
        data: b"FRAME 2\n".to_vec(),
    };

    let mut buf = Vec::new();
    ttyman::write_frame(&mut buf, &frame1).unwrap();
    ttyman::write_frame(&mut buf, &frame2).unwrap();
    std::fs::write(&file_path, buf).unwrap();

    let mut player_child = Command::new(bin)
        .arg("play")
        .arg(&file_path)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn ttyproxy play");

    std::thread::sleep(std::time::Duration::from_millis(100));

    // Send SIGINT (Ctrl-C) to player
    let pid = Pid::from_raw(player_child.id() as i32);
    let _ = kill(pid, Signal::SIGINT);

    let status = player_child.wait().unwrap();
    assert!(!status.success()); // Terminated by SIGINT
}

#[test]
fn test_watch_e2e() {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;

    let bin = env!("CARGO_BIN_EXE_ttyman");

    // Start a cat session with ttyproxy run in PTY mode
    let mut run_child = Command::new(bin)
        .arg("run")
        .arg("-T")
        .arg("always")
        .arg("--")
        .arg("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn ttyproxy run");

    let run_pid = run_child.id();

    // Wait for IPC socket
    let sock_path = ttyman::default_socket_path(run_pid).unwrap();
    for _ in 0..50 {
        if sock_path.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(sock_path.exists(), "IPC socket was not created in time");

    // Spawn ttyproxy watch -s <pid>
    let watch_child = Command::new(bin)
        .arg("watch")
        .arg("-s")
        .arg(run_pid.to_string())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn ttyproxy watch");

    std::thread::sleep(std::time::Duration::from_millis(100));

    // Write text into session via write command
    let send_status = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(run_pid.to_string())
        .arg("-E")
        .arg("LIVE STREAM WATCH VERIFIED")
        .status()
        .expect("failed to write text");
    assert!(send_status.success());

    std::thread::sleep(std::time::Duration::from_millis(100));

    // Terminate watch with SIGINT
    let watch_pid = Pid::from_raw(watch_child.id() as i32);
    let _ = kill(watch_pid, Signal::SIGINT);

    let watch_out = watch_child.wait_with_output().unwrap();
    let watch_str = String::from_utf8_lossy(&watch_out.stdout);
    assert!(
        watch_str.contains("LIVE STREAM WATCH VERIFIED"),
        "expected live stream text in watch output, got: {watch_str}"
    );

    // Clean up run_child
    let _ = kill(Pid::from_raw(run_pid as i32), Signal::SIGTERM);
    let _ = run_child.wait();
}

#[test]
fn test_attach_interactive_flow_e2e() {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    use std::io::Write;

    let bin = env!("CARGO_BIN_EXE_ttyman");

    // Start a cat session with tp run
    let mut run_child = Command::new(bin)
        .arg("run")
        .arg("-T")
        .arg("always")
        .arg("--")
        .arg("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn tp run");

    let run_pid = run_child.id();

    // Wait for IPC socket
    let sock_path = ttyman::default_socket_path(run_pid).unwrap();
    for _ in 0..50 {
        if sock_path.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(sock_path.exists(), "IPC socket was not created in time");

    // Spawn tp attach -s <pid> (using default out-of-the-box echo detach, isolated from user config)
    let mut attach_child = Command::new(bin)
        .arg("attach")
        .arg("-c")
        .arg("/dev/null")
        .arg("-s")
        .arg(run_pid.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn tp attach");

    std::thread::sleep(std::time::Duration::from_millis(100));

    // Send input via attach's stdin
    if let Some(ref mut stdin) = attach_child.stdin {
        stdin
            .write_all(b"ATTACH_INTERACTIVE_INPUT\n")
            .expect("failed to write to attach stdin");
        stdin.flush().expect("failed to flush attach stdin");
    }

    std::thread::sleep(std::time::Duration::from_millis(400));

    // Send Ctrl-] (0x1D) to trigger default detach
    if let Some(ref mut stdin) = attach_child.stdin {
        stdin.write_all(&[0x1D]).expect("failed to send menu key");
        stdin.flush().expect("failed to flush menu key");
    }

    // Attach should exit cleanly with success (0)
    let attach_out = attach_child
        .wait_with_output()
        .expect("attach failed to exit");
    assert!(attach_out.status.success());
    let attach_str = String::from_utf8_lossy(&attach_out.stdout);
    assert!(
        attach_str.contains("ATTACH_INTERACTIVE_INPUT"),
        "expected echoed input in attach stdout, got: {attach_str}"
    );

    // Verify tp run session is still alive and responsive via tp read
    let read_out = Command::new(bin)
        .arg("read")
        .arg("-s")
        .arg(run_pid.to_string())
        .output()
        .expect("failed to run tp read");
    let read_str = String::from_utf8_lossy(&read_out.stdout);
    assert!(
        read_str.contains("ATTACH_INTERACTIVE_INPUT"),
        "expected session to have processed input, got: {read_str}"
    );

    // Clean up run_child
    let _ = kill(Pid::from_raw(run_pid as i32), Signal::SIGTERM);
    let _ = run_child.wait();
}

#[test]
fn test_attach_custom_menu_key_e2e() {
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    use std::io::Write;

    let bin = env!("CARGO_BIN_EXE_ttyman");

    // Start a cat session with tp run
    let mut run_child = Command::new(bin)
        .arg("run")
        .arg("-T")
        .arg("always")
        .arg("--")
        .arg("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn tp run");

    let run_pid = run_child.id();

    // Wait for IPC socket
    let sock_path = ttyman::default_socket_path(run_pid).unwrap();
    for _ in 0..50 {
        if sock_path.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(sock_path.exists(), "IPC socket was not created in time");

    // Spawn tp attach with custom chord in config file (Ctrl-p, Ctrl-q)
    let cfg_path = std::env::temp_dir().join(format!("tt_cfg_custom_{}.toml", std::process::id()));
    std::fs::write(
        &cfg_path,
        "[menu]\nkey = \"0x10,0x11\"\ncommand = \"echo detach\"\n",
    )
    .unwrap();

    let mut attach_child = Command::new(bin)
        .arg("attach")
        .arg("-c")
        .arg(&cfg_path)
        .arg("-s")
        .arg(run_pid.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn tp attach with custom menu key");

    std::thread::sleep(std::time::Duration::from_millis(100));

    // Send ctrl-p ctrl-q (0x10, 0x11) to detach
    if let Some(ref mut stdin) = attach_child.stdin {
        stdin
            .write_all(&[0x10, 0x11])
            .expect("failed to send custom chord");
        stdin.flush().expect("failed to flush custom chord");
    }

    // Attach should exit cleanly with success (0)
    let attach_out = attach_child
        .wait_with_output()
        .expect("attach failed to exit");
    assert!(attach_out.status.success());
    let _ = std::fs::remove_file(&cfg_path);

    // Clean up run_child
    let _ = kill(Pid::from_raw(run_pid as i32), Signal::SIGTERM);
    let _ = run_child.wait();
}

#[test]
fn test_start_named_session_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");
    let session_name = format!("tsess{}", std::process::id() % 10000);

    // 1. Spawn background named session via ttyman start -s <NAME>
    let output = Command::new(bin)
        .arg("start")
        .arg("-s")
        .arg(&session_name)
        .arg("--")
        .arg("sh")
        .output()
        .expect("failed to execute ttyman start");

    assert!(output.status.success());
    let stdout_str = String::from_utf8_lossy(&output.stdout);
    assert!(stdout_str.contains("Started ttyman session in background"));

    let sock_path = ttyman::ipc::named_socket_path(&session_name).unwrap();
    for _ in 0..50 {
        if sock_path.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(sock_path.exists(), "Socket path was not created");

    // 2. Write command into background session using session name
    let write_res = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(&session_name)
        .arg("-E")
        .arg("echo 'TP_ATTACH_AUTO_SPAWN_VERIFIED'")
        .output()
        .expect("failed to write to session");
    assert!(write_res.status.success());

    std::thread::sleep(std::time::Duration::from_millis(300));

    // 3. Read screen snapshot from session using session name
    let read_res = Command::new(bin)
        .arg("read")
        .arg("-s")
        .arg(&session_name)
        .output()
        .expect("failed to read from session");
    assert!(read_res.status.success());
    let screen = String::from_utf8_lossy(&read_res.stdout);
    assert!(
        screen.contains("TP_ATTACH_AUTO_SPAWN_VERIFIED"),
        "Screen did not contain echoed text: {screen}"
    );

    // 4. Verify in tp list (both table and json)
    let list_res = Command::new(bin)
        .arg("list")
        .output()
        .expect("failed to list sessions");
    assert!(list_res.status.success());
    let list_str = String::from_utf8_lossy(&list_res.stdout);
    assert!(list_str.contains(&session_name));

    let json_res = Command::new(bin)
        .arg("list")
        .arg("--json")
        .output()
        .expect("failed to list sessions in json");
    assert!(json_res.status.success());
    let json_str = String::from_utf8_lossy(&json_res.stdout);
    assert!(json_str.contains(&format!("\"name\": \"{session_name}\"")));

    // 5. Clean exit
    let _ = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(&session_name)
        .arg("-E")
        .arg("exit")
        .output();
}

#[test]
fn test_attach_auto_spawn_and_interactive_detach_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");
    let session_name = format!("test-auto-{}", std::process::id());

    // Spawn tp attach -s <NAME> -- sh (spawns and attaches immediately with default echo detach)
    let mut child = Command::new(bin)
        .arg("attach")
        .arg("-c")
        .arg("/dev/null")
        .arg("-s")
        .arg(&session_name)
        .arg("--")
        .arg("sh")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn tp attach");

    std::thread::sleep(std::time::Duration::from_millis(300));

    // Send Ctrl-] (0x1D) to detach
    if let Some(ref mut stdin) = child.stdin {
        stdin.write_all(&[0x1D]).expect("failed to send menu key");
        stdin.flush().expect("failed to flush menu key");
    }

    let out = child
        .wait_with_output()
        .expect("tp attach failed to exit cleanly");
    assert!(out.status.success());

    // Clean up background session
    let _ = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(&session_name)
        .arg("-E")
        .arg("exit")
        .output();
}

#[test]
fn test_start_without_socket_arg_uses_pid_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");

    // Spawn ttyman start -- sh (without -s argument)
    let output = Command::new(bin)
        .env_remove("TTYMAN_SESSION")
        .env_remove("TTYMAN_PID")
        .arg("start")
        .arg("--")
        .arg("sh")
        .output()
        .expect("failed to spawn ttyman start without -s");

    assert!(output.status.success());
    let stdout_str = String::from_utf8_lossy(&output.stdout);
    assert!(stdout_str.contains("Started ttyman session in background"));

    // Extract socket path from output: (socket: /path/to/ttyman-<PID>)
    let socket_str = stdout_str
        .lines()
        .find(|line| line.contains("socket: "))
        .and_then(|line| line.split("socket: ").nth(1))
        .map(|s| s.trim_end_matches(')'))
        .expect("socket path not found in output");

    let sock_path = std::path::PathBuf::from(socket_str);
    assert!(sock_path.exists());
    let filename = sock_path.file_name().unwrap().to_str().unwrap();
    assert!(filename.starts_with("ttyman-"));

    // Verify name inside socket is a PID (all digits)
    let pid_str = filename.strip_prefix("ttyman-").unwrap();
    assert!(
        pid_str.parse::<u32>().is_ok(),
        "Socket should use numeric PID: {}",
        pid_str
    );

    // Clean up background session
    let _ = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(pid_str)
        .arg("-E")
        .arg("exit")
        .output();
}

#[test]
fn test_run_named_socket_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");
    let session_name = format!("trun{}", std::process::id() % 10000);

    // Spawn ttyman run -s <NAME> -- sh
    let mut run_child = Command::new(bin)
        .arg("run")
        .arg("-s")
        .arg(&session_name)
        .arg("--")
        .arg("sh")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("failed to spawn ttyman run with named socket");

    let sock_path = ttyman::ipc::named_socket_path(&session_name).unwrap();
    for _ in 0..50 {
        if sock_path.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        sock_path.exists(),
        "Socket path was not created for ttyman run -s"
    );

    // Verify we can read from the named socket
    let read_res = Command::new(bin)
        .arg("read")
        .arg("-s")
        .arg(&session_name)
        .output()
        .expect("failed to read from named ttyman run session");
    assert!(read_res.status.success());

    // Write exit into run_child via ttyman write
    let write_res = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(&session_name)
        .arg("-E")
        .arg("exit 42")
        .output()
        .expect("failed to write to named ttyman run session");
    assert!(write_res.status.success());

    let exit_status = run_child.wait().expect("run_child failed to exit");
    assert_eq!(exit_status.code(), Some(42));
}

#[test]
fn test_self_attach_prevention_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");
    let session_name = format!("tself{}", std::process::id() % 10000);

    // Simulate being inside session_name by setting TTYMAN_SESSION
    let output = Command::new(bin)
        .env("TTYMAN_SESSION", &session_name)
        .arg("attach")
        .output()
        .expect("failed to execute ttyman attach with TTYMAN_SESSION");

    assert!(!output.status.success());
    let stderr_str = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr_str.contains("Cannot attach to session"),
        "Expected error message not found in stderr: {}",
        stderr_str
    );

    // Same with explicit -s session_name when TTYMAN_SESSION matches
    let output2 = Command::new(bin)
        .env("TTYMAN_SESSION", &session_name)
        .arg("attach")
        .arg("-s")
        .arg(&session_name)
        .output()
        .expect("failed to execute ttyman attach -s matching TTYMAN_SESSION");

    assert!(!output2.status.success());
    let stderr_str2 = String::from_utf8_lossy(&output2.stderr);
    assert!(
        stderr_str2.contains("Cannot attach to session"),
        "Expected error message not found in stderr: {}",
        stderr_str2
    );
}

#[test]
fn test_nested_attach_auto_background_spawn_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");
    let sess_a = format!("t_nest_a_{}", std::process::id() % 10000);
    let sess_b = format!("t_nest_b_{}", std::process::id() % 10000);
    let sock_b = ttyman::ipc::named_socket_path(&sess_b).unwrap();

    // 1. Simulate being inside session A (TTYMAN_SESSION=sess_a), and running `ttyman attach -s sess_b`
    let out_spawn = Command::new(bin)
        .env("TTYMAN_SESSION", &sess_a)
        .arg("attach")
        .arg("-s")
        .arg(&sess_b)
        .arg("--")
        .arg("sh")
        .output()
        .expect("run nested attach to sess_b");

    assert!(out_spawn.status.success());
    let stdout_str = String::from_utf8_lossy(&out_spawn.stdout);
    assert!(
        stdout_str.contains("Started session"),
        "Expected started background message, got: {stdout_str}"
    );
    assert!(
        stdout_str.contains("nesting prevented"),
        "Expected nesting prevented notice, got: {stdout_str}"
    );
    assert!(sock_b.exists(), "Session B socket should exist");

    // 2. Running `ttyman attach -s sess_b` again while inside session A notices it's already running
    let out_already = Command::new(bin)
        .env("TTYMAN_SESSION", &sess_a)
        .arg("attach")
        .arg("-s")
        .arg(&sess_b)
        .output()
        .expect("run nested attach to existing sess_b");

    assert!(out_already.status.success());
    let stdout_str2 = String::from_utf8_lossy(&out_already.stdout);
    assert!(
        stdout_str2.contains("is already running in background"),
        "Expected already running notice, got: {stdout_str2}"
    );
    assert!(
        stdout_str2.contains("nesting prevented"),
        "Expected nesting prevented notice, got: {stdout_str2}"
    );

    // Clean up
    let _ = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(&sess_b)
        .arg("-E")
        .arg("exit")
        .output();
    let _ = Command::new(bin).arg("list").arg("--clean").output();
}

#[test]
fn test_self_attach_prevention_by_physical_tty_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");
    let name1 = format!("tpty1_{}", std::process::id() % 10000);
    let name2 = format!("tpty2_{}", std::process::id() % 10000);

    // 1. Spawn sess1 and sess2 in background
    let out1 = Command::new(bin)
        .arg("start")
        .arg("-s")
        .arg(&name1)
        .arg("--")
        .arg("sh")
        .output()
        .expect("spawn name1");
    assert!(out1.status.success());

    let out2 = Command::new(bin)
        .arg("start")
        .arg("-s")
        .arg(&name2)
        .arg("--")
        .arg("sh")
        .output()
        .expect("spawn name2");
    assert!(out2.status.success());

    // 2. Query info for sess1 to get its pty_slave_path
    let sock1 = ttyman::ipc::named_socket_path(&name1).unwrap();
    let info1 = ttyman::ipc::query_session_info(&sock1).expect("query session 1");
    let slave_path = info1.pty_slave_path.expect("slave_path for session 1");

    // 3. Open slave_path directly and try to attach to name1 WITHOUT TTYMAN_SOCK
    let slave_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&slave_path)
        .expect("open slave pty");

    let output = Command::new(bin)
        .env_remove("TTYMAN_SESSION")
        .env_remove("TTYMAN_PID")
        .arg("attach")
        .arg("-s")
        .arg(&name1)
        .stdin(slave_file.try_clone().unwrap())
        .stdout(slave_file.try_clone().unwrap())
        .stderr(std::process::Stdio::piped())
        .output()
        .expect("run attach to self PTY");

    assert!(!output.status.success());
    let stderr_str = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr_str.contains("from within its own terminal"),
        "Expected physical TTY error message, got: {stderr_str}"
    );

    // Cleanup
    let _ = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(&name1)
        .arg("-E")
        .arg("exit")
        .output();
    let _ = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(&name2)
        .arg("-E")
        .arg("exit")
        .output();
    let _ = Command::new(bin).arg("list").arg("--clean").output();
}

#[test]
fn test_attach_interactive_menu_switch_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");
    let name1 = format!("tswch1_{}", std::process::id() % 10000);
    let name2 = format!("tswch2_{}", std::process::id() % 10000);
    let test_dir = tempfile::tempdir().expect("create temp dir");

    // 1. Spawn sess1 and sess2 in background
    let out1 = Command::new(bin)
        .env("XDG_RUNTIME_DIR", test_dir.path())
        .arg("start")
        .arg("-s")
        .arg(&name1)
        .arg("--")
        .arg("sh")
        .output()
        .expect("spawn name1");
    assert!(out1.status.success());

    let out2 = Command::new(bin)
        .env("XDG_RUNTIME_DIR", test_dir.path())
        .arg("start")
        .arg("-s")
        .arg(&name2)
        .arg("--")
        .arg("sh")
        .output()
        .expect("spawn name2");
    assert!(out2.status.success());

    // 2. Spawn client attached to name1 with config file that outputs attach:name2
    let cfg_path = test_dir.path().join("tt_cfg_switch.toml");
    std::fs::write(
        &cfg_path,
        format!("[menu]\ncommand = \"echo attach:{name2}\"\n"),
    )
    .unwrap();

    let mut client = Command::new(bin)
        .env("XDG_RUNTIME_DIR", test_dir.path())
        .arg("attach")
        .arg("-c")
        .arg(&cfg_path)
        .arg("-s")
        .arg(&name1)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn client");

    std::thread::sleep(std::time::Duration::from_millis(400));

    // Verify name1 has 1 client
    let list1 = Command::new(bin)
        .env("XDG_RUNTIME_DIR", test_dir.path())
        .arg("list")
        .arg("--json")
        .output()
        .expect("list json");
    let s1_list: Vec<serde_json::Value> =
        serde_json::from_str(&String::from_utf8_lossy(&list1.stdout)).unwrap();
    let s1_item = s1_list.iter().find(|s| s["name"] == name1).unwrap();
    assert_eq!(s1_item["clients"], 1);

    // 3. Send Ctrl-] (0x1D) to trigger menu_command
    if let Some(ref mut stdin) = client.stdin {
        use std::io::Write;
        stdin.write_all(&[0x1D]).expect("send menu key");
        stdin.flush().expect("flush menu key");
    }

    // 4. Verify name1 now has 0 clients and name2 has 1 client!
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2500);
    let mut switch_ok = false;
    while std::time::Instant::now() < deadline {
        let list2 = Command::new(bin)
            .env("XDG_RUNTIME_DIR", test_dir.path())
            .arg("list")
            .arg("--json")
            .output()
            .expect("list json");
        if let Ok(s2_list) =
            serde_json::from_str::<Vec<serde_json::Value>>(&String::from_utf8_lossy(&list2.stdout))
        {
            let s1_item = s2_list.iter().find(|s| s["name"] == name1);
            let s2_item = s2_list.iter().find(|s| s["name"] == name2);
            if let (Some(s1), Some(s2)) = (s1_item, s2_item)
                && s1["clients"] == 0
                && s2["clients"] == 1
            {
                switch_ok = true;
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        switch_ok,
        "name1 should have 0 clients and name2 should have 1 client after switch"
    );

    // Clean up
    let _ = client.kill();
    let _ = client.wait();
    let _ = Command::new(bin)
        .env("XDG_RUNTIME_DIR", test_dir.path())
        .arg("write")
        .arg("-s")
        .arg(&name1)
        .arg("-E")
        .arg("exit")
        .output();
    let _ = Command::new(bin)
        .env("XDG_RUNTIME_DIR", test_dir.path())
        .arg("write")
        .arg("-s")
        .arg(&name2)
        .arg("-E")
        .arg("exit")
        .output();
    let _ = Command::new(bin).arg("list").arg("--clean").output();
}

#[test]
fn test_attach_interactive_menu_dynamic_create_new_session_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");
    let name1 = format!("tswdyn1_{}", std::process::id() % 10000);
    let name_new = format!("tswdyn_new_{}", std::process::id() % 10000);
    let _sock_new = ttyman::ipc::named_socket_path(&name_new).unwrap();

    // 1. Spawn sess1 in background
    let out1 = Command::new(bin)
        .arg("start")
        .arg("-s")
        .arg(&name1)
        .arg("--")
        .arg("sh")
        .output()
        .expect("spawn name1");
    assert!(out1.status.success());

    // 2. Spawn client attached to name1 with config file that outputs attach:name_new (brand new session)
    let cfg_path = std::env::temp_dir().join(format!("tt_cfg_dyn_{}.toml", std::process::id()));
    std::fs::write(
        &cfg_path,
        format!("[menu]\ncommand = \"echo attach:{name_new}\"\n"),
    )
    .unwrap();

    let mut client = Command::new(bin)
        .arg("attach")
        .arg("-c")
        .arg(&cfg_path)
        .arg("-s")
        .arg(&name1)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn client");

    std::thread::sleep(std::time::Duration::from_millis(400));

    // 3. Send Ctrl-] (0x1D) to trigger menu_command -> switches to brand new session!
    if let Some(ref mut stdin) = client.stdin {
        use std::io::Write;
        stdin.write_all(&[0x1D]).expect("send menu key");
        stdin.flush().expect("flush menu key");
    }

    // 4. Verify name_new was automatically spawned and has 1 client!
    let mut s_new = None;
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(2500);
    while std::time::Instant::now() < deadline {
        let list = Command::new(bin)
            .arg("list")
            .arg("--json")
            .output()
            .expect("list json");
        if let Ok(s_list) =
            serde_json::from_str::<Vec<serde_json::Value>>(&String::from_utf8_lossy(&list.stdout))
            && let Some(item) = s_list.iter().find(|s| s["name"] == name_new)
            && item["clients"] == 1
        {
            s_new = Some(item.clone());
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert!(
        s_new.is_some(),
        "New session '{name_new}' should appear in list with 1 connected client"
    );

    // Clean up
    let _ = client.kill();
    let _ = client.wait();
    let _ = std::fs::remove_file(&cfg_path);
    let _ = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(&name1)
        .arg("-E")
        .arg("exit")
        .output();
    let _ = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(&name_new)
        .arg("-E")
        .arg("exit")
        .output();
    let _ = Command::new(bin).arg("list").arg("--clean").output();
}

#[test]
fn test_attach_interactive_menu_cancel_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");
    let name = format!("tswcan_{}", std::process::id() % 10000);

    // 1. Spawn session
    let out = Command::new(bin)
        .arg("start")
        .arg("-s")
        .arg(&name)
        .arg("--")
        .arg("sh")
        .output()
        .expect("spawn name");
    assert!(out.status.success());

    // 2. Spawn client with config file that fails/cancels (exit 130 like fzf ESC)
    let cfg_path = std::env::temp_dir().join(format!("tt_cfg_can_{}.toml", std::process::id()));
    std::fs::write(&cfg_path, "[menu]\ncommand = \"exit 130\"\n").unwrap();

    let mut client = Command::new(bin)
        .arg("attach")
        .arg("-c")
        .arg(&cfg_path)
        .arg("-s")
        .arg(&name)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn client");

    std::thread::sleep(std::time::Duration::from_millis(400));

    // 3. Trigger menu (Ctrl-] / 0x1D)
    if let Some(ref mut stdin) = client.stdin {
        use std::io::Write;
        stdin.write_all(&[0x1D]).expect("send menu key");
        stdin.flush().expect("flush menu key");
    }

    std::thread::sleep(std::time::Duration::from_millis(500));

    // 4. Verify session screen: shell must NOT contain any JSON or command not found error
    let read_out = Command::new(bin)
        .arg("read")
        .arg("-s")
        .arg(&name)
        .arg("-a")
        .output()
        .expect("read session");
    let screen = String::from_utf8_lossy(&read_out.stdout);
    assert!(
        !screen.contains("Attach") && !screen.contains("command not found"),
        "Session screen contains unexpected phantom input on menu cancel: {screen}"
    );

    // Clean up
    let _ = client.kill();
    let _ = client.wait();
    let _ = std::fs::remove_file(&cfg_path);
    let _ = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(&name)
        .arg("-E")
        .arg("exit")
        .output();
    let _ = Command::new(bin).arg("list").arg("--clean").output();
}

#[test]
fn test_attach_auto_switch_on_session_termination_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");
    let name1 = format!("tsw_main_{}", std::process::id() % 10000);
    let name2 = format!("tsw_work_{}", std::process::id() % 10000);

    // 1. Spawn sess1 and sess2 in background running sh
    let out1 = Command::new(bin)
        .arg("start")
        .arg("-s")
        .arg(&name1)
        .arg("--")
        .arg("sh")
        .output()
        .expect("spawn name1");
    assert!(out1.status.success());

    let out2 = Command::new(bin)
        .arg("start")
        .arg("-s")
        .arg(&name2)
        .arg("--")
        .arg("sh")
        .output()
        .expect("spawn name2");
    assert!(out2.status.success());

    // 2. Spawn client attached to name1 with config file that switches to name2
    let cfg_path = std::env::temp_dir().join(format!("tt_cfg_autosw_{}.toml", std::process::id()));
    std::fs::write(
        &cfg_path,
        format!("[menu]\ncommand = \"echo attach:{name2}\"\n"),
    )
    .unwrap();

    let mut client = Command::new(bin)
        .arg("attach")
        .arg("-c")
        .arg(&cfg_path)
        .arg("-s")
        .arg(&name1)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn client");

    std::thread::sleep(std::time::Duration::from_millis(400));

    // 3. Switch to name2 using Ctrl-] (0x1D)
    if let Some(ref mut stdin) = client.stdin {
        use std::io::Write;
        stdin.write_all(&[0x1D]).expect("send menu key");
        stdin.flush().expect("flush menu key");
    }

    std::thread::sleep(std::time::Duration::from_millis(500));

    // 4. Send "exit\n" to name2 (terminating name2)
    if let Some(ref mut stdin) = client.stdin {
        use std::io::Write;
        stdin.write_all(b"exit\n").expect("send exit to name2");
        stdin.flush().expect("flush exit");
    }

    // Wait for name2 to exit and ttyman client to auto-switch back to name1 (LRU)
    std::thread::sleep(std::time::Duration::from_millis(700));

    // 5. Send command to name1 through the client
    if let Some(ref mut stdin) = client.stdin {
        use std::io::Write;
        stdin
            .write_all(b"echo BACK_IN_NAME1_OK\n")
            .expect("send cmd to name1");
        stdin.flush().expect("flush cmd");
    }

    std::thread::sleep(std::time::Duration::from_millis(400));

    let read_out = Command::new(bin)
        .arg("read")
        .arg("-s")
        .arg(&name1)
        .arg("-a")
        .output()
        .expect("read name1");
    let screen = String::from_utf8_lossy(&read_out.stdout);
    assert!(
        screen.contains("BACK_IN_NAME1_OK"),
        "Client should be back in name1 after name2 exited! Got screen:\n{screen}"
    );

    // Clean up
    let _ = client.kill();
    let _ = client.wait();
    let _ = std::fs::remove_file(&cfg_path);
    let _ = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(&name1)
        .arg("-E")
        .arg("exit")
        .output();
    let _ = Command::new(bin).arg("list").arg("--clean").output();
}

#[test]
fn test_attach_sigwinch_does_not_leak_json_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");
    let name = format!("tsw_winch_{}", std::process::id() % 10000);

    // 1. Spawn sess in background running sh
    let out = Command::new(bin)
        .arg("start")
        .arg("-s")
        .arg(&name)
        .arg("--")
        .arg("sh")
        .output()
        .expect("spawn name");
    assert!(out.status.success());

    // 2. Spawn interactive client
    let mut client = Command::new(bin)
        .arg("attach")
        .arg("-s")
        .arg(&name)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn client");

    std::thread::sleep(std::time::Duration::from_millis(400));

    // 3. Send SIGWINCH signal to client process
    use nix::sys::signal::{Signal, kill};
    use nix::unistd::Pid;
    let _ = kill(Pid::from_raw(client.id() as i32), Signal::SIGWINCH);

    std::thread::sleep(std::time::Duration::from_millis(400));

    // 4. Verify session screen does NOT contain "Resize" JSON
    let read_out = Command::new(bin)
        .arg("read")
        .arg("-s")
        .arg(&name)
        .arg("-a")
        .output()
        .expect("read name");
    let screen = String::from_utf8_lossy(&read_out.stdout);
    assert!(
        !screen.contains("Resize"),
        "Screen should not contain leaked Resize JSON on SIGWINCH! Screen:\n{screen}"
    );

    // Clean up
    let _ = client.kill();
    let _ = client.wait();
    let _ = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(&name)
        .arg("-E")
        .arg("exit")
        .output();
    let _ = Command::new(bin).arg("list").arg("--clean").output();
}

#[test]
fn test_rename_session_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");
    let old_name = format!("ts_old_{}", std::process::id() % 10000);
    let new_name = format!("ts_new_{}", std::process::id() % 10000);

    let old_sock = ttyman::ipc::named_socket_path(&old_name).unwrap();
    let new_sock = ttyman::ipc::named_socket_path(&new_name).unwrap();

    // 1. Start session with old name
    let out = Command::new(bin)
        .arg("start")
        .arg("-s")
        .arg(&old_name)
        .arg("--")
        .arg("sh")
        .output()
        .expect("start session");
    assert!(out.status.success());
    assert!(old_sock.exists(), "Old socket should exist");

    // 2. Rename session to new name
    let out_rename = Command::new(bin)
        .arg("rename")
        .arg("-s")
        .arg(&old_name)
        .arg(&new_name)
        .output()
        .expect("rename session");
    assert!(out_rename.status.success());
    let stdout_str = String::from_utf8_lossy(&out_rename.stdout);
    assert!(stdout_str.contains(&format!("Renamed session to '{new_name}'")));

    // 3. Verify old socket is gone and new socket exists
    assert!(!old_sock.exists(), "Old socket should no longer exist");
    assert!(new_sock.exists(), "New socket should exist");

    // 4. Verify ttyman list contains new session name and not old
    let out_list = Command::new(bin)
        .arg("list")
        .arg("--json")
        .output()
        .expect("list sessions");
    let json_list: Vec<serde_json::Value> =
        serde_json::from_str(&String::from_utf8_lossy(&out_list.stdout)).unwrap();
    assert!(json_list.iter().any(|s| s["name"] == new_name));
    assert!(!json_list.iter().any(|s| s["name"] == old_name));

    // 5. Test IPC interaction on new socket name
    let _ = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(&new_name)
        .arg("-E")
        .arg("echo HELLO_AFTER_RENAME")
        .output();
    std::thread::sleep(std::time::Duration::from_millis(300));

    let out_read = Command::new(bin)
        .arg("read")
        .arg("-s")
        .arg(&new_name)
        .arg("-a")
        .output()
        .expect("read new session");
    let screen = String::from_utf8_lossy(&out_read.stdout);
    assert!(screen.contains("HELLO_AFTER_RENAME"));

    // 6. Clean up
    let _ = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(&new_name)
        .arg("-E")
        .arg("exit")
        .output();
    let _ = Command::new(bin).arg("list").arg("--clean").output();
}

#[test]
fn test_rename_from_inside_session_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");
    let old_name = format!("ts_in_old_{}", std::process::id() % 10000);
    let new_name = format!("ts_in_new_{}", std::process::id() % 10000);

    let old_sock = ttyman::ipc::named_socket_path(&old_name).unwrap();
    let new_sock = ttyman::ipc::named_socket_path(&new_name).unwrap();

    // 1. Start session
    let out = Command::new(bin)
        .arg("start")
        .arg("-s")
        .arg(&old_name)
        .arg("--")
        .arg("sh")
        .output()
        .expect("start session");
    assert!(out.status.success());

    // Query supervisor PID before rename
    let info = ttyman::ipc::query_session_info(&old_sock).expect("query session info");
    let sup_pid = info.pid;

    // 2. Rename without -s, using TTYMAN_SESSION
    let out_rename = Command::new(bin)
        .env("TTYMAN_SESSION", &old_name)
        .arg("rename")
        .arg(&new_name)
        .output()
        .expect("rename with TTYMAN_SESSION");
    assert!(out_rename.status.success());
    assert!(!old_sock.exists());
    assert!(new_sock.exists());

    // 3. Verify that commands inside the session with stale TTYMAN_SESSION and valid TTYMAN_PID
    // still automatically resolve the renamed session socket without needing -s
    let out_write = Command::new(bin)
        .env("TTYMAN_SESSION", &old_name)
        .env("TTYMAN_PID", sup_pid.to_string())
        .arg("write")
        .arg("-E")
        .arg("echo TTYMAN_PID_FALLBACK_WORKS")
        .output()
        .expect("write with stale TTYMAN_SESSION and valid TTYMAN_PID");
    assert!(out_write.status.success(), "failed: {:?}", String::from_utf8_lossy(&out_write.stderr));

    // Clean up
    let _ = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(&new_name)
        .arg("-E")
        .arg("exit")
        .output();
    let _ = Command::new(bin).arg("list").arg("--clean").output();
}

#[test]
fn test_rename_conflict_fails_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");
    let name1 = format!("ts_cfl1_{}", std::process::id() % 10000);
    let name2 = format!("ts_cfl2_{}", std::process::id() % 10000);

    // 1. Start both sessions
    let out1 = Command::new(bin)
        .arg("start")
        .arg("-s")
        .arg(&name1)
        .arg("--")
        .arg("sh")
        .output()
        .expect("start 1");
    assert!(out1.status.success());

    let out2 = Command::new(bin)
        .arg("start")
        .arg("-s")
        .arg(&name2)
        .arg("--")
        .arg("sh")
        .output()
        .expect("start 2");
    assert!(out2.status.success());

    // 2. Attempt to rename name1 to name2 (conflict)
    let out_rename = Command::new(bin)
        .arg("rename")
        .arg("-s")
        .arg(&name1)
        .arg(&name2)
        .output()
        .expect("rename conflict");
    assert!(!out_rename.status.success(), "Should fail on conflict");
    let err_str = String::from_utf8_lossy(&out_rename.stderr);
    assert!(
        err_str.contains("already exists"),
        "Expected conflict error message, got: {err_str}"
    );

    // Clean up
    let _ = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(&name1)
        .arg("-E")
        .arg("exit")
        .output();
    let _ = Command::new(bin)
        .arg("write")
        .arg("-s")
        .arg(&name2)
        .arg("-E")
        .arg("exit")
        .output();
    let _ = Command::new(bin).arg("list").arg("--clean").output();
}

#[test]
fn test_attach_auto_switch_fallback_to_other_active_session_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");
    let name_a = format!("tsw_fb_a_{}", std::process::id() % 10000);
    let name_b = format!("tsw_fb_b_{}", std::process::id() % 10000);
    let test_dir = tempfile::tempdir().expect("create temp dir");

    // 1. Start sess A and sess B in background
    let out_a = Command::new(bin)
        .env("XDG_RUNTIME_DIR", test_dir.path())
        .arg("start")
        .arg("-s")
        .arg(&name_a)
        .arg("--")
        .arg("sh")
        .output()
        .expect("start A");
    assert!(out_a.status.success());

    let out_b = Command::new(bin)
        .env("XDG_RUNTIME_DIR", test_dir.path())
        .arg("start")
        .arg("-s")
        .arg(&name_b)
        .arg("--")
        .arg("sh")
        .output()
        .expect("start B");
    assert!(out_b.status.success());

    // 2. Attach directly to B (client has never visited A in its history stack)
    let mut client = Command::new(bin)
        .env("XDG_RUNTIME_DIR", test_dir.path())
        .arg("attach")
        .arg("-s")
        .arg(&name_b)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn client");

    std::thread::sleep(std::time::Duration::from_millis(400));

    // 3. Send exit to B -> B terminates
    if let Some(ref mut stdin) = client.stdin {
        use std::io::Write;
        stdin.write_all(b"exit\n").expect("exit B");
        stdin.flush().expect("flush exit");
    }

    // Wait for client to automatically switch fallback to A
    std::thread::sleep(std::time::Duration::from_millis(600));

    // 4. Send command to A through client
    if let Some(ref mut stdin) = client.stdin {
        use std::io::Write;
        stdin
            .write_all(b"echo IN_A_AFTER_FALLBACK_OK\n")
            .expect("send cmd to A");
        stdin.flush().expect("flush cmd");
    }

    std::thread::sleep(std::time::Duration::from_millis(400));

    let read_out = Command::new(bin)
        .env("XDG_RUNTIME_DIR", test_dir.path())
        .arg("read")
        .arg("-s")
        .arg(&name_a)
        .arg("-a")
        .output()
        .expect("read A");
    let screen = String::from_utf8_lossy(&read_out.stdout);
    assert!(
        screen.contains("IN_A_AFTER_FALLBACK_OK"),
        "Client should switch to session A when B ends! Screen:\n{screen}"
    );

    // Clean up
    let _ = client.kill();
    let _ = client.wait();
    let _ = Command::new(bin)
        .env("XDG_RUNTIME_DIR", test_dir.path())
        .arg("write")
        .arg("-s")
        .arg(&name_a)
        .arg("-E")
        .arg("exit")
        .output();
}

#[test]
fn test_attach_menu_recent_sessions_env_var_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");
    let name1 = format!("ts_rec1_{}", std::process::id() % 10000);
    let name2 = format!("ts_rec2_{}", std::process::id() % 10000);
    let test_dir = tempfile::tempdir().expect("create temp dir");
    let rec_file = test_dir.path().join("recent_output.txt");

    // 1. Start sess1 and sess2 in background
    let out1 = Command::new(bin)
        .env("XDG_RUNTIME_DIR", test_dir.path())
        .arg("start")
        .arg("-s")
        .arg(&name1)
        .arg("--")
        .arg("sh")
        .output()
        .expect("start 1");
    assert!(out1.status.success());

    let out2 = Command::new(bin)
        .env("XDG_RUNTIME_DIR", test_dir.path())
        .arg("start")
        .arg("-s")
        .arg(&name2)
        .arg("--")
        .arg("sh")
        .output()
        .expect("start 2");
    assert!(out2.status.success());

    // 2. Config file that writes $TTYMAN_RECENT_SESSIONS to rec_file and switches to name2
    let cfg_path = test_dir.path().join("cfg_rec.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "[menu]\ncommand = \"echo \\\"$TTYMAN_RECENT_SESSIONS\\\" > '{}'; echo attach:{name2}\"\n",
            rec_file.display()
        ),
    )
    .unwrap();

    // 3. Attach to name1
    let mut client = Command::new(bin)
        .env("XDG_RUNTIME_DIR", test_dir.path())
        .arg("attach")
        .arg("-c")
        .arg(&cfg_path)
        .arg("-s")
        .arg(&name1)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn client");

    std::thread::sleep(std::time::Duration::from_millis(400));

    // 4. Trigger menu key (0x1D) in name1
    if let Some(ref mut stdin) = client.stdin {
        use std::io::Write;
        stdin.write_all(&[0x1D]).expect("send menu key");
        stdin.flush().expect("flush menu key");
    }

    std::thread::sleep(std::time::Duration::from_millis(600));

    // Check recent sessions in step 1
    let out_text1 = std::fs::read_to_string(&rec_file).unwrap_or_default();
    assert_eq!(
        out_text1.trim(),
        name1,
        "First menu trigger should have only name1 in TTYMAN_RECENT_SESSIONS"
    );

    // 5. Update config to switch back to name1 or detach
    std::fs::write(
        &cfg_path,
        format!(
            "[menu]\ncommand = \"echo \\\"$TTYMAN_RECENT_SESSIONS\\\" > '{}'; echo detach\"\n",
            rec_file.display()
        ),
    )
    .unwrap();

    // 6. Trigger menu key (0x1D) while inside name2
    if let Some(ref mut stdin) = client.stdin {
        use std::io::Write;
        stdin.write_all(&[0x1D]).expect("send menu key 2");
        stdin.flush().expect("flush menu key 2");
    }

    std::thread::sleep(std::time::Duration::from_millis(600));

    // Check recent sessions in step 2 (should be "name2 name1")
    let out_text2 = std::fs::read_to_string(&rec_file).unwrap_or_default();
    let expected_recent = format!("{name2} {name1}");
    assert_eq!(
        out_text2.trim(),
        expected_recent,
        "Second menu trigger should have 'name2 name1' in TTYMAN_RECENT_SESSIONS"
    );

    // Clean up
    let _ = client.kill();
    let _ = client.wait();
    let _ = Command::new(bin)
        .env("XDG_RUNTIME_DIR", test_dir.path())
        .arg("write")
        .arg("-s")
        .arg(&name1)
        .arg("-E")
        .arg("exit")
        .output();
    let _ = Command::new(bin)
        .env("XDG_RUNTIME_DIR", test_dir.path())
        .arg("write")
        .arg("-s")
        .arg(&name2)
        .arg("-E")
        .arg("exit")
        .output();
}

#[test]
fn test_session_name_validation_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");

    let invalid_names = [
        "../traversal",
        "foo/bar",
        "foo\\bar",
        "has space",
        "foo.sock",
        "-leadingdash",
        ".",
        "..",
        "semicolon;rm",
        "$dollar",
    ];

    for name in invalid_names {
        // ttyman start --session=<invalid> should fail
        let out_start = Command::new(bin)
            .arg("start")
            .arg(format!("--session={name}"))
            .output()
            .expect("start with invalid name");
        assert!(
            !out_start.status.success(),
            "Expected start with '{name}' to fail"
        );
        let err = String::from_utf8_lossy(&out_start.stderr);
        assert!(
            err.contains("Invalid session name")
                || err.contains("cannot start with")
                || err.contains("is reserved")
                || err.contains("cannot end with '.sock'"),
            "Unexpected error message for '{name}': {err}"
        );

        // ttyman read --session=<invalid> should fail
        let out_read = Command::new(bin)
            .arg("read")
            .arg(format!("--session={name}"))
            .output()
            .expect("read with invalid name");
        assert!(
            !out_read.status.success(),
            "Expected read with '{name}' to fail"
        );

        // ttyman rename -s valid_sess <invalid> should fail
        let out_rename = Command::new(bin)
            .arg("rename")
            .arg("-s")
            .arg("valid_sess")
            .arg(name)
            .output()
            .expect("rename with invalid name");
        assert!(
            !out_rename.status.success(),
            "Expected rename with '{name}' to fail"
        );
    }
}

#[test]
fn test_fallback_to_tmp_user_dir_without_xdg_runtime_dir_e2e() {
    let bin = env!("CARGO_BIN_EXE_ttyman");
    let uid = nix::unistd::geteuid().as_raw();
    let expected_fallback = std::path::PathBuf::from(format!("/tmp/ttyman-{uid}"));

    // 1. ttyman list succeeds without XDG_RUNTIME_DIR and creates/uses /tmp/ttyman-<UID>
    let out_list = Command::new(bin)
        .env_remove("XDG_RUNTIME_DIR")
        .arg("list")
        .output()
        .expect("list without XDG_RUNTIME_DIR");
    assert!(
        out_list.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out_list.stderr)
    );
    assert!(expected_fallback.exists(), "Expected {:?} to exist", expected_fallback);

    // Verify permissions are 0700
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(&expected_fallback).expect("metadata");
    assert_eq!(meta.permissions().mode() & 0o777, 0o700);
}


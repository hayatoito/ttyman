use nix::sys::signal::{Signal, kill};
use nix::unistd::Pid;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_ttyman");

struct TestEnv {
    dir: tempfile::TempDir,
}

impl TestEnv {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("create temp dir"),
        }
    }

    fn file_path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn sock_path(&self, name: &str) -> PathBuf {
        self.dir.path().join(format!("ttyman-{name}"))
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(BIN);
        c.env("XDG_RUNTIME_DIR", self.dir.path());
        c.env_remove("TTYMAN_SESSION");
        c.env_remove("TTYMAN_PID");
        c
    }

    fn run(&self, args: &[&str]) -> String {
        let mut cmd = self.cmd();
        cmd.args(args);
        let out = cmd
            .output()
            .unwrap_or_else(|e| panic!("run {:?}: {e}", args));
        assert!(
            out.status.success(),
            "Command {:?} failed: status={}, stderr={}",
            args,
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn run_fail(&self, args: &[&str]) -> String {
        let mut cmd = self.cmd();
        cmd.args(args);
        let out = cmd
            .output()
            .unwrap_or_else(|e| panic!("run {:?}: {e}", args));
        assert!(
            !out.status.success(),
            "Expected {:?} to fail: {:?}",
            args,
            out
        );
        String::from_utf8_lossy(&out.stderr).to_string()
    }

    fn run_with_env(&self, args: &[&str], envs: &[(&str, Option<&str>)]) -> String {
        let mut cmd = self.cmd();
        for (k, v) in envs {
            if let Some(val) = v {
                cmd.env(k, val);
            } else {
                cmd.env_remove(k);
            }
        }
        cmd.args(args);
        let out = cmd
            .output()
            .unwrap_or_else(|e| panic!("run {:?}: {e}", args));
        assert!(
            out.status.success(),
            "Command {:?} failed: stderr={}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn run_fail_with_env(&self, args: &[&str], envs: &[(&str, Option<&str>)]) -> String {
        let mut cmd = self.cmd();
        for (k, v) in envs {
            if let Some(val) = v {
                cmd.env(k, val);
            } else {
                cmd.env_remove(k);
            }
        }
        cmd.args(args);
        let out = cmd
            .output()
            .unwrap_or_else(|e| panic!("run {:?}: {e}", args));
        assert!(!out.status.success(), "Expected {:?} to fail", args);
        String::from_utf8_lossy(&out.stderr).to_string()
    }

    fn wait_path(&self, path: &Path) -> bool {
        for _ in 0..50 {
            if path.exists() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        path.exists()
    }

    fn wait_screen_contains(&self, session: &str, needle: &str) -> bool {
        for _ in 0..50 {
            if let Ok(out) = self.cmd().args(["read", "-s", session]).output()
                && String::from_utf8_lossy(&out.stdout).contains(needle)
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        false
    }

    fn list_json(&self) -> Vec<serde_json::Value> {
        let out = self.run(&["list", "--json"]);
        serde_json::from_str(&out).unwrap_or_default()
    }

    fn wait_session_clients(&self, name: &str, expected: usize, timeout_ms: u64) -> bool {
        let deadline = Instant::now() + Duration::from_millis(timeout_ms);
        while Instant::now() < deadline {
            let json = self.list_json();
            if let Some(item) = json.iter().find(|s| s["name"] == name)
                && item["clients"] == expected
            {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        false
    }

    fn spawn_attach(&self, session: &str, config: Option<&Path>) -> Child {
        let mut cmd = self.cmd();
        cmd.arg("attach");
        if let Some(cfg) = config {
            cmd.arg("-c").arg(cfg);
        } else {
            cmd.arg("-c").arg("/dev/null");
        }
        cmd.arg("-s").arg(session);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn attach")
    }

    fn send_input(child: &mut Child, data: &[u8]) {
        if let Some(ref mut stdin) = child.stdin {
            let _ = stdin.write_all(data);
            let _ = stdin.flush();
        }
    }
}

#[test]
fn test_read_outside_session() {
    let env = TestEnv::new();
    let err = env.run_fail_with_env(&["read"], &[("TTYMAN_SESSION", None), ("TTYMAN_PID", None)]);
    assert!(err.contains("Not running inside a 'ttyman' session"));
}

#[test]
fn test_read_inside_session() {
    let env = TestEnv::new();
    let capture_out = env.file_path("captured.txt");
    let script = format!(
        "echo 'HEADER_LINE_1'; echo 'DATA_LINE_2'; sleep 0.2; '{BIN}' read > '{}'",
        capture_out.display()
    );

    env.run(&["start", "-s", "read_in_sess", "--", "sh", "-c", &script]);

    let deadline = Instant::now() + Duration::from_millis(2000);
    let mut content = String::new();
    while Instant::now() < deadline {
        if let Ok(c) = std::fs::read_to_string(&capture_out)
            && c.contains("HEADER_LINE_1")
        {
            content = c;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    assert!(content.contains("HEADER_LINE_1"));
    assert!(content.contains("DATA_LINE_2"));
}

#[test]
fn test_read_all_scrollback_e2e() {
    let env = TestEnv::new();
    let name = "t_read_all";

    env.run(&["start", "-s", name, "--", "sh"]);
    assert!(env.wait_path(&env.sock_path(name)));

    // Emit 100 lines into session
    let cmd = "for i in $(seq 1 100); do echo \"E2E_LINE_$i\"; done";
    env.run(&["write", "-s", name, "-E", cmd]);
    assert!(env.wait_screen_contains(name, "E2E_LINE_100"));

    // Default read (visible screen only) should only contain recent lines
    let visible = env.run(&["read", "-s", name]);
    assert!(visible.contains("E2E_LINE_100"));
    assert!(!visible.contains("E2E_LINE_1\n"));

    // `read -a` / `read --all` must contain the entire history from Line 1 to Line 100
    let all_out = env.run(&["read", "-s", name, "-a"]);
    assert!(
        all_out.contains("E2E_LINE_1\n"),
        "Missing E2E_LINE_1 in all_out"
    );
    assert!(all_out.contains("E2E_LINE_50\n"));
    assert!(all_out.contains("E2E_LINE_100"));

    let _ = env.cmd().args(["write", "-s", name, "-E", "exit"]).status();
}

#[test]
fn test_write_and_read_e2e() {
    let env = TestEnv::new();
    let name = "t_write_read";
    let out_file = env.file_path("injected_out.txt");

    env.run(&["start", "-s", name, "--", "sh"]);
    assert!(env.wait_path(&env.sock_path(name)));

    let cmd_str = format!("echo 'INJECTED_CMD_SUCCESS' > '{}'", out_file.display());
    env.run(&["write", "-s", name, "-E", &cmd_str]);
    assert!(env.wait_path(&out_file));
    assert!(
        std::fs::read_to_string(&out_file)
            .unwrap()
            .contains("INJECTED_CMD_SUCCESS")
    );

    let _ = env.cmd().args(["write", "-s", name, "-E", "exit"]).status();
}

#[test]
fn test_list_e2e() {
    let env = TestEnv::new();
    let name = "t_list_sess";
    env.run(&["start", "-s", name, "--", "sh"]);
    assert!(env.wait_path(&env.sock_path(name)));

    let list_str = env.run(&["list"]);
    assert!(list_str.contains(name));

    let json_str = env.run(&["list", "--json"]);
    assert!(json_str.contains(&format!("\"name\": \"{name}\"")));
    assert!(json_str.contains("\"is_alive\": true"));

    let _ = env.cmd().args(["write", "-s", name, "-E", "exit"]).status();
}

#[test]
fn test_attach_byte_passthrough_e2e() {
    let env = TestEnv::new();
    let out_bin = env.file_path("received_attach.bin");
    let session_name = "t_att_passthrough";

    let script = format!(
        "python3 -c \"import sys; data = sys.stdin.buffer.read(5); open('{}', 'wb').write(data)\"",
        out_bin.display()
    );

    env.run(&["start", "-s", session_name, "--", "sh", "-c", &script]);
    assert!(env.wait_path(&env.sock_path(session_name)));

    let mut attach = env.spawn_attach(session_name, None);
    std::thread::sleep(Duration::from_millis(200));

    let raw_payload = &[0x02, 0x06, 0x07, b'A', b'\n'];
    TestEnv::send_input(&mut attach, raw_payload);
    drop(attach.stdin.take());

    let _ = attach.wait();
    std::thread::sleep(Duration::from_millis(300));

    assert!(out_bin.exists());
    assert_eq!(std::fs::read(&out_bin).unwrap(), raw_payload);
}

#[test]
fn test_watch_e2e() {
    let env = TestEnv::new();
    let name = "t_watch_sess";
    env.run(&["start", "-s", name, "--", "sh"]);
    assert!(env.wait_path(&env.sock_path(name)));

    let watch_child = env
        .cmd()
        .args(["watch", "-s", name])
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn watch");

    std::thread::sleep(Duration::from_millis(100));
    env.run(&[
        "write",
        "-s",
        name,
        "-E",
        "echo 'LIVE STREAM WATCH VERIFIED'",
    ]);
    std::thread::sleep(Duration::from_millis(100));

    let _ = kill(Pid::from_raw(watch_child.id() as i32), Signal::SIGINT);
    let watch_out = watch_child.wait_with_output().unwrap();
    assert!(String::from_utf8_lossy(&watch_out.stdout).contains("LIVE STREAM WATCH VERIFIED"));

    let _ = env.run(&["kill", "-s", name]);
}

#[test]
fn test_attach_interactive_flow_e2e() {
    let env = TestEnv::new();
    let name = "t_att_flow";
    env.run(&["start", "-s", name, "--", "sh"]);
    assert!(env.wait_path(&env.sock_path(name)));

    let mut attach = env.spawn_attach(name, None);
    std::thread::sleep(Duration::from_millis(100));

    TestEnv::send_input(&mut attach, b"echo ATTACH_INTERACTIVE_INPUT\n");
    std::thread::sleep(Duration::from_millis(400));
    TestEnv::send_input(&mut attach, &[0x1D]);

    let attach_out = attach.wait_with_output().expect("attach exit");
    assert!(attach_out.status.success());
    assert!(String::from_utf8_lossy(&attach_out.stdout).contains("ATTACH_INTERACTIVE_INPUT"));

    let read_out = env.run(&["read", "-s", name]);
    assert!(read_out.contains("ATTACH_INTERACTIVE_INPUT"));

    let _ = env.run(&["kill", "-s", name]);
}

#[test]
fn test_attach_custom_menu_key_e2e() {
    let env = TestEnv::new();
    let name = "t_custom_menu";
    env.run(&["start", "-s", name, "--", "sh"]);
    assert!(env.wait_path(&env.sock_path(name)));

    let cfg_path = env.file_path("custom_menu.toml");
    std::fs::write(&cfg_path, "[menu]\nkey = 0x02\ncommand = \"echo detach\"\n").unwrap();

    let mut attach = env.spawn_attach(name, Some(&cfg_path));
    std::thread::sleep(Duration::from_millis(100));

    TestEnv::send_input(&mut attach, &[0x02]);
    let attach_out = attach.wait_with_output().expect("attach exit");
    assert!(attach_out.status.success());

    let _ = env.run(&["kill", "-s", name]);
}

#[test]
fn test_start_named_session_e2e() {
    let env = TestEnv::new();
    let name = "tsess_start";

    let out = env.run(&["start", "-s", name, "--", "sh"]);
    assert!(out.contains("Started session 'tsess_start' in background"));
    assert!(env.wait_path(&env.sock_path(name)));

    env.run(&["write", "-s", name, "-E", "echo 'AUTO_SPAWN_VERIFIED'"]);
    assert!(env.wait_screen_contains(name, "AUTO_SPAWN_VERIFIED"));

    let list_str = env.run(&["list"]);
    assert!(list_str.contains(name));

    let json_str = env.run(&["list", "--json"]);
    assert!(json_str.contains(&format!("\"name\": \"{name}\"")));

    let _ = env.cmd().args(["write", "-s", name, "-E", "exit"]).status();
}

#[test]
fn test_attach_auto_spawn_and_interactive_detach_e2e() {
    let env = TestEnv::new();
    let name = "test-auto-spawn";

    let mut child = env.spawn_attach(name, None);
    std::thread::sleep(Duration::from_millis(300));

    TestEnv::send_input(&mut child, &[0x1D]);
    let out = child.wait_with_output().expect("attach exit");
    assert!(out.status.success());

    let _ = env.cmd().args(["write", "-s", name, "-E", "exit"]).status();
}

#[test]
fn test_start_without_session_arg_uses_pid_e2e() {
    let env = TestEnv::new();
    let out = env.run_with_env(
        &["start", "--", "sh"],
        &[("TTYMAN_SESSION", None), ("TTYMAN_PID", None)],
    );
    assert!(out.contains("Started session '"));

    let session_name = out
        .lines()
        .find(|l| l.contains("Started session '"))
        .and_then(|l| l.split("Started session '").nth(1))
        .and_then(|l| l.split('\'').next())
        .expect("session name");

    assert!(session_name.parse::<u32>().is_ok());

    let _ = env
        .cmd()
        .args(["write", "-s", session_name, "-E", "exit"])
        .status();
}

#[test]
fn test_self_attach_prevention_e2e() {
    let env = TestEnv::new();
    let name = "tself_prev";

    let err1 = env.run_fail_with_env(&["attach"], &[("TTYMAN_SESSION", Some(name))]);
    assert!(err1.contains("Already in session"));

    let err2 = env.run_fail_with_env(&["attach", "-s", name], &[("TTYMAN_SESSION", Some(name))]);
    assert!(err2.contains("Already in session"));
}

#[test]
fn test_nested_attach_auto_background_spawn_e2e() {
    let env = TestEnv::new();
    let sess_a = "t_nest_a";
    let sess_b = "t_nest_b";

    let out1 = env.run_with_env(
        &["attach", "-s", sess_b, "--", "sh"],
        &[("TTYMAN_SESSION", Some(sess_a))],
    );
    assert!(out1.contains("Cannot attach to session 't_nest_b' from inside a ttyman session"));
    assert!(env.wait_path(&env.sock_path(sess_b)));

    let out2 = env.run_with_env(
        &["attach", "-s", sess_b],
        &[("TTYMAN_SESSION", Some(sess_a))],
    );
    assert!(out2.contains("Cannot attach to session 't_nest_b' from inside a ttyman session"));

    let _ = env
        .cmd()
        .args(["write", "-s", sess_b, "-E", "exit"])
        .status();
}

#[test]
fn test_self_attach_prevention_by_physical_tty_e2e() {
    let env = TestEnv::new();
    let name1 = "tpty1";
    let name2 = "tpty2";

    env.run(&["start", "-s", name1, "--", "sh"]);
    env.run(&["start", "-s", name2, "--", "sh"]);

    let sock1 = env.sock_path(name1);
    let info1 = ttyman::ipc::query_session_info(&sock1).expect("query session 1");
    let slave = info1.pty_slave_path.as_deref().expect("slave path");

    let slave_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(slave)
        .expect("open slave");

    let output = env
        .cmd()
        .env_remove("TTYMAN_SESSION")
        .env_remove("TTYMAN_PID")
        .args(["attach", "-s", name1])
        .stdin(slave_file.try_clone().unwrap())
        .stdout(slave_file)
        .output()
        .expect("attach");

    assert!(!output.status.success());
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("Already in session"));

    let _ = env
        .cmd()
        .args(["write", "-s", name1, "-E", "exit"])
        .status();
    let _ = env
        .cmd()
        .args(["write", "-s", name2, "-E", "exit"])
        .status();
}

#[test]
fn test_attach_interactive_menu_switch_e2e() {
    let env = TestEnv::new();
    let name1 = "tswch1";
    let name2 = "tswch2";

    env.run(&["start", "-s", name1, "--", "sh"]);
    env.run(&["start", "-s", name2, "--", "sh"]);

    let cfg_path = env.file_path("switch.toml");
    std::fs::write(
        &cfg_path,
        format!("[menu]\ncommand = \"echo attach {name2}\"\n"),
    )
    .unwrap();

    let mut client = env.spawn_attach(name1, Some(&cfg_path));
    std::thread::sleep(Duration::from_millis(400));
    assert!(env.wait_session_clients(name1, 1, 1000));

    TestEnv::send_input(&mut client, &[0x1D]);
    assert!(env.wait_session_clients(name1, 0, 2500));
    assert!(env.wait_session_clients(name2, 1, 2500));

    let _ = client.kill();
    let _ = env
        .cmd()
        .args(["write", "-s", name1, "-E", "exit"])
        .status();

    let _ = env
        .cmd()
        .args(["write", "-s", name2, "-E", "exit"])
        .status();
}

#[test]
fn test_attach_interactive_menu_attach_anonymous_e2e() {
    let env = TestEnv::new();
    let name1 = "t_menu_anon1";

    env.run(&["start", "-s", name1, "--", "sh"]);

    let cfg_path = env.file_path("attach_anon.toml");
    std::fs::write(&cfg_path, "[menu]\ncommand = \"echo attach\"\n").unwrap();

    let mut client = env.spawn_attach(name1, Some(&cfg_path));
    std::thread::sleep(Duration::from_millis(400));
    assert!(env.wait_session_clients(name1, 1, 1000));

    // Press Ctrl-] to trigger menu with "echo attach"
    TestEnv::send_input(&mut client, &[0x1D]);
    assert!(env.wait_session_clients(name1, 0, 2500));

    // A new session should be spawned and attached (total sessions == 2)
    std::thread::sleep(Duration::from_millis(500));
    let socks: Vec<_> = std::fs::read_dir(env.dir.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("ttyman-"))
        .collect();
    assert_eq!(socks.len(), 2);

    let _ = client.kill();
    let _ = client.wait();
    let _ = env
        .cmd()
        .args(["write", "-s", name1, "-E", "exit"])
        .status();
}

#[test]
fn test_attach_interactive_menu_dynamic_create_new_session_e2e() {
    let env = TestEnv::new();
    let name1 = "tswdyn1";
    let name_new = "tswdyn_new";

    env.run(&["start", "-s", name1, "--", "sh"]);

    let cfg_path = env.file_path("dynamic.toml");
    std::fs::write(
        &cfg_path,
        format!("[menu]\ncommand = \"echo attach {name_new}\"\n"),
    )
    .unwrap();

    let mut client = env.spawn_attach(name1, Some(&cfg_path));
    std::thread::sleep(Duration::from_millis(400));

    TestEnv::send_input(&mut client, &[0x1D]);
    assert!(env.wait_session_clients(name_new, 1, 3000));

    let _ = client.kill();
    let _ = client.wait();
    let _ = env
        .cmd()
        .args(["write", "-s", name1, "-E", "exit"])
        .status();
    let _ = env
        .cmd()
        .args(["write", "-s", name_new, "-E", "exit"])
        .status();
}

#[test]
fn test_attach_interactive_menu_write_passthrough_e2e() {
    let env = TestEnv::new();
    let name = "tsw_write_pt";

    env.run(&["start", "-s", name, "--", "sh"]);

    // Configure menu to send raw byte 0x1D into session via printf | ttyman write
    let cfg_path = env.file_path("write_cmd.toml");
    std::fs::write(
        &cfg_path,
        format!("[menu]\ncommand = \"printf '\\\\x1d' | {BIN} write\"\n"),
    )
    .unwrap();

    let mut client = env.spawn_attach(name, Some(&cfg_path));
    std::thread::sleep(Duration::from_millis(400));
    assert!(env.wait_session_clients(name, 1, 1000));

    // Trigger menu -> invokes `printf '\x1d' | ttyman write`
    TestEnv::send_input(&mut client, &[0x1D]);
    std::thread::sleep(Duration::from_millis(500));

    // Client should still be attached to 'name'
    assert!(env.wait_session_clients(name, 1, 1000));

    let _ = client.kill();
    let _ = client.wait();
    let _ = env.cmd().args(["write", "-s", name, "-E", "exit"]).status();
}

#[test]
fn test_attach_interactive_menu_cancel_e2e() {
    let env = TestEnv::new();
    let name = "tswcan";

    env.run(&["start", "-s", name, "--", "sh"]);

    let cfg_path = env.file_path("cancel.toml");
    std::fs::write(&cfg_path, "[menu]\ncommand = \"exit 130\"\n").unwrap();

    let mut client = env.spawn_attach(name, Some(&cfg_path));
    std::thread::sleep(Duration::from_millis(400));

    TestEnv::send_input(&mut client, &[0x1D]);
    std::thread::sleep(Duration::from_millis(500));

    let screen = env.run(&["read", "-s", name, "-a"]);
    assert!(!screen.contains("Attach") && !screen.contains("command not found"));

    let _ = client.kill();
    let _ = client.wait();
    let _ = env.cmd().args(["write", "-s", name, "-E", "exit"]).status();
}

#[test]
fn test_attach_auto_switch_on_session_termination_e2e() {
    let env = TestEnv::new();
    let name1 = "tsw_term1";
    let name2 = "tsw_term2";

    env.run(&["start", "-s", name1, "--", "sh"]);
    env.run(&["start", "-s", name2, "--", "sh"]);

    let cfg_path = env.file_path("autosw.toml");
    std::fs::write(
        &cfg_path,
        format!("[menu]\ncommand = \"echo attach {name2}\"\n"),
    )
    .unwrap();

    let mut client = env.spawn_attach(name1, Some(&cfg_path));
    std::thread::sleep(Duration::from_millis(400));

    TestEnv::send_input(&mut client, &[0x1D]);
    std::thread::sleep(Duration::from_millis(500));

    TestEnv::send_input(&mut client, b"exit\n");
    std::thread::sleep(Duration::from_millis(700));

    TestEnv::send_input(&mut client, b"echo BACK_IN_NAME1_OK\n");
    std::thread::sleep(Duration::from_millis(400));

    let screen = env.run(&["read", "-s", name1, "-a"]);
    assert!(screen.contains("BACK_IN_NAME1_OK"));

    let _ = client.kill();
    let _ = client.wait();
    let _ = env
        .cmd()
        .args(["write", "-s", name1, "-E", "exit"])
        .status();
}

#[test]
fn test_attach_sigwinch_does_not_leak_json_e2e() {
    let env = TestEnv::new();
    let name = "tsw_winch";

    env.run(&["start", "-s", name, "--", "sh"]);

    let mut client = env.spawn_attach(name, None);
    std::thread::sleep(Duration::from_millis(400));

    let _ = kill(Pid::from_raw(client.id() as i32), Signal::SIGWINCH);
    std::thread::sleep(Duration::from_millis(400));

    let screen = env.run(&["read", "-s", name, "-a"]);
    assert!(!screen.contains("Resize"));

    let _ = client.kill();
    let _ = client.wait();
    let _ = env.cmd().args(["write", "-s", name, "-E", "exit"]).status();
}

#[test]
fn test_rename_session_e2e() {
    let env = TestEnv::new();
    let old_name = "ts_rename_old";
    let new_name = "ts_rename_new";

    env.run(&["start", "-s", old_name, "--", "sh"]);
    let old_sock = env.sock_path(old_name);
    let new_sock = env.sock_path(new_name);
    assert!(env.wait_path(&old_sock));

    let out = env.run(&["rename", "-s", old_name, new_name]);
    assert!(out.contains(&format!("Renamed session to '{new_name}'")));
    assert!(!old_sock.exists());
    assert!(new_sock.exists());

    let json = env.list_json();
    assert!(json.iter().any(|s| s["name"] == new_name));
    assert!(!json.iter().any(|s| s["name"] == old_name));

    env.run(&["write", "-s", new_name, "-E", "echo HELLO_AFTER_RENAME"]);
    assert!(env.wait_screen_contains(new_name, "HELLO_AFTER_RENAME"));

    let _ = env
        .cmd()
        .args(["write", "-s", new_name, "-E", "exit"])
        .status();
}

#[test]
fn test_rename_from_inside_session_e2e() {
    let env = TestEnv::new();
    let old_name = "ts_in_old";
    let new_name = "ts_in_new";

    env.run(&["start", "-s", old_name, "--", "sh"]);
    let old_sock = env.sock_path(old_name);
    let new_sock = env.sock_path(new_name);
    assert!(env.wait_path(&old_sock));

    let out = env.run_with_env(&["rename", new_name], &[("TTYMAN_SESSION", Some(old_name))]);
    assert!(out.contains(&format!("Renamed session to '{new_name}'")));
    assert!(!old_sock.exists());
    assert!(new_sock.exists());

    let _ = env
        .cmd()
        .args(["write", "-s", new_name, "-E", "exit"])
        .status();
}

#[test]
fn test_rename_conflict_fails_e2e() {
    let env = TestEnv::new();
    let name_a = "ts_conf_a";
    let name_b = "ts_conf_b";

    env.run(&["start", "-s", name_a, "--", "sh"]);
    env.run(&["start", "-s", name_b, "--", "sh"]);

    let err = env.run_fail(&["rename", "-s", name_a, name_b]);
    assert!(err.contains("already exists"));
    assert!(env.sock_path(name_a).exists());
    assert!(env.sock_path(name_b).exists());

    let _ = env
        .cmd()
        .args(["write", "-s", name_a, "-E", "exit"])
        .status();
    let _ = env
        .cmd()
        .args(["write", "-s", name_b, "-E", "exit"])
        .status();
}

#[test]
fn test_attach_auto_switch_fallback_to_other_active_session_e2e() {
    let env = TestEnv::new();
    let name_a = "tsw_fb_a";
    let name_b = "tsw_fb_b";

    env.run(&["start", "-s", name_a, "--", "sh"]);
    env.run(&["start", "-s", name_b, "--", "sh"]);

    let mut client = env.spawn_attach(name_b, None);
    std::thread::sleep(Duration::from_millis(400));

    TestEnv::send_input(&mut client, b"exit\n");
    std::thread::sleep(Duration::from_millis(600));

    TestEnv::send_input(&mut client, b"echo IN_A_AFTER_FALLBACK_OK\n");
    std::thread::sleep(Duration::from_millis(400));

    let screen = env.run(&["read", "-s", name_a, "-a"]);
    assert!(screen.contains("IN_A_AFTER_FALLBACK_OK"));

    let _ = client.kill();
    let _ = client.wait();
    let _ = env
        .cmd()
        .args(["write", "-s", name_a, "-E", "exit"])
        .status();
}

#[test]
fn test_attach_menu_recent_sessions_env_var_e2e() {
    let env = TestEnv::new();
    let name1 = "ts_mru1";
    let name2 = "ts_mru2";
    let rec_file = env.file_path("recent_output.txt");

    env.run(&["start", "-s", name1, "--", "sh"]);
    env.run(&["start", "-s", name2, "--", "sh"]);

    let cfg_path = env.file_path("mru.toml");
    std::fs::write(
        &cfg_path,
        format!(
            "[menu]\ncommand = \"echo \\\"$TTYMAN_RECENT_SESSIONS\\\" > '{}'; echo attach {name2}\"\n",
            rec_file.display()
        ),
    )
    .unwrap();

    let mut client = env.spawn_attach(name1, Some(&cfg_path));
    std::thread::sleep(Duration::from_millis(400));

    TestEnv::send_input(&mut client, &[0x1D]);
    std::thread::sleep(Duration::from_millis(600));

    assert!(env.wait_path(&rec_file));
    let rec_content = std::fs::read_to_string(&rec_file).unwrap();
    assert!(rec_content.trim() == name1);

    let cfg2_path = env.file_path("mru2.toml");
    std::fs::write(
        &cfg2_path,
        format!(
            "[menu]\ncommand = \"echo \\\"$TTYMAN_RECENT_SESSIONS\\\" > '{}'; echo detach\"\n",
            rec_file.display()
        ),
    )
    .unwrap();

    let mut client2 = env.spawn_attach(name2, Some(&cfg2_path));
    std::thread::sleep(Duration::from_millis(400));

    TestEnv::send_input(&mut client2, &[0x1D]);
    std::thread::sleep(Duration::from_millis(600));

    let rec_content2 = std::fs::read_to_string(&rec_file).unwrap();
    assert!(rec_content2.trim() == name2);

    let _ = client.kill();
    let _ = client.wait();
    let _ = client2.kill();
    let _ = client2.wait();
    let _ = env
        .cmd()
        .args(["write", "-s", name1, "-E", "exit"])
        .status();
    let _ = env
        .cmd()
        .args(["write", "-s", name2, "-E", "exit"])
        .status();
}

#[test]
fn test_session_name_validation_e2e() {
    let env = TestEnv::new();
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
        let err1 = env.run_fail(&["start", &format!("--session={name}")]);
        assert!(
            err1.contains("Invalid session name")
                || err1.contains("cannot start with")
                || err1.contains("is reserved")
                || err1.contains("cannot end with '.sock'")
        );

        let _ = env.run_fail(&["read", &format!("--session={name}")]);
        let _ = env.run_fail(&["rename", "-s", "valid_sess", name]);
    }
}

#[test]
fn test_fallback_to_tmp_user_dir_without_xdg_runtime_dir_e2e() {
    let uid = nix::unistd::geteuid().as_raw();
    let expected_fallback = PathBuf::from(format!("/tmp/ttyman-{uid}"));

    let env = TestEnv::new();
    env.run_with_env(&["list"], &[("XDG_RUNTIME_DIR", None)]);
    assert!(expected_fallback.exists());

    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(&expected_fallback).expect("metadata");
    assert_eq!(meta.permissions().mode() & 0o777, 0o700);
}

#[test]
fn test_external_sigterm_cleanup_and_notification_e2e() {
    let env = TestEnv::new();
    let name = "tsigterm_clean";

    env.run(&["start", "-s", name, "--", "sh"]);

    let watcher = env
        .cmd()
        .args(["watch", "-s", name])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("watcher");

    std::thread::sleep(Duration::from_millis(150));

    let json = env.list_json();
    let daemon_pid = json
        .iter()
        .find(|s| s["name"] == name)
        .and_then(|s| s["pid"].as_i64())
        .expect("daemon pid");

    let _ = kill(Pid::from_raw(daemon_pid as i32), Signal::SIGTERM);

    let watcher_out = watcher.wait_with_output().expect("watcher exit");
    assert!(
        String::from_utf8_lossy(&watcher_out.stdout)
            .contains("[ttyman: Session terminated by SIGTERM]")
    );

    std::thread::sleep(Duration::from_millis(100));
    let json_after = env.list_json();
    assert!(
        !json_after
            .iter()
            .any(|s| s["name"] == name && s["is_alive"] == true)
    );
}

#[test]
fn test_kill_subcommand_e2e() {
    let env = TestEnv::new();
    let name1 = "tkill1";
    let name2 = "tkill2";

    env.run(&["start", "-s", name1, "--", "sh"]);
    env.run(&["start", "-s", name2, "--", "sh"]);

    let k1 = env.run(&["kill", "-s", name1]);
    assert!(k1.contains(&format!("Killed session '{name1}'")));

    let json1 = env.list_json();
    assert!(
        !json1
            .iter()
            .any(|s| s["name"] == name1 && s["is_alive"] == true)
    );

    let k1_again = env.run_fail(&["kill", "-s", name1]);
    assert!(k1_again.contains("not found"));

    let k2 = env.run(&["kill", "-s", name2]);
    assert!(k2.contains(&format!("Killed session '{name2}'")));

    let json2 = env.list_json();
    assert!(
        !json2
            .iter()
            .any(|s| s["name"] == name2 && s["is_alive"] == true)
    );

    env.run_fail(&["kill", "unexpected_pos_arg"]);
}

#[test]
fn test_list_current_session_indicator_e2e() {
    let env = TestEnv::new();
    let name1 = "tcur1";
    let name2 = "tcur2";

    env.run(&["start", "-s", name1, "--", "sh"]);
    env.run(&["start", "-s", name2, "--", "sh"]);

    let str_none = env.run_with_env(&["list"], &[("TTYMAN_SESSION", None)]);
    assert!(!str_none.contains(&format!("* {name1}")));
    assert!(!str_none.contains(&format!("* {name2}")));

    let str_cur = env.run_with_env(&["list"], &[("TTYMAN_SESSION", Some(name1))]);
    assert!(str_cur.contains(&format!("* {name1}")));
    assert!(!str_cur.contains(&format!("* {name2}")));

    let json_str = env.run_with_env(&["list", "--json"], &[("TTYMAN_SESSION", Some(name1))]);
    let json: Vec<serde_json::Value> = serde_json::from_str(&json_str).unwrap();
    let e1 = json.iter().find(|e| e["name"] == name1).unwrap();
    let e2 = json.iter().find(|e| e["name"] == name2).unwrap();
    assert_eq!(e1["is_current"], true);
    assert_eq!(e2["is_current"], false);
}

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

fn private_command(program: &str, home: &Path, socket: &Path) -> Command {
    let mut command = Command::new(program);
    command
        .env_clear()
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join("config"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("LIMUX_SOCKET", socket)
        .env("PATH", "/usr/bin:/bin");
    command
}

/// A hook run from a Limux terminal, which names its tab.
fn terminal_command(program: &str, home: &Path, socket: &Path) -> Command {
    let mut command = private_command(program, home, socket);
    command.env("LIMUX_TAB_ID", "tab-a");
    command
}

fn notification_params(mut command: Command, socket: &Path, payload: Value) -> Value {
    let listener = UnixListener::bind(socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut stream = loop {
        match listener.accept() {
            Ok((stream, _)) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let output = child.wait_with_output().unwrap();
                    panic!("hook sent no notification: {output:?}");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("accept hook request: {error}"),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut line = String::new();
    BufReader::new(stream.try_clone().unwrap())
        .read_line(&mut line)
        .unwrap();
    let request: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(request["method"], "notification.create");
    writeln!(
        stream,
        "{}",
        json!({"id": request["id"], "ok": true, "result": {}})
    )
    .unwrap();
    // A hook from a terminal also reports the agent's state: answer those
    // calls until it exits. The client reuses its connection, so they may
    // come on this one or on a new one.
    std::thread::spawn(move || answer_ok(stream));
    while child.try_wait().unwrap().is_none() {
        match listener.accept() {
            Ok((stream, _)) => {
                std::thread::spawn(move || answer_ok(stream));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    panic!("hook did not exit");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("accept hook request: {error}"),
        }
    }
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{output:?}");
    std::fs::remove_file(socket).unwrap();
    request["params"].clone()
}

/// Answers every request on a connection with an empty success, until the
/// client closes it.
fn answer_ok(mut stream: std::os::unix::net::UnixStream) {
    stream.set_nonblocking(false).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut line = String::new();
    while matches!(reader.read_line(&mut line), Ok(read) if read > 0) {
        let request: Value = serde_json::from_str(&line).unwrap();
        if writeln!(
            stream,
            "{}",
            json!({"id": request["id"], "ok": true, "result": {}})
        )
        .is_err()
        {
            return;
        }
        line.clear();
    }
}

#[test]
fn installed_claude_hooks_distinguish_notifications_from_completion() {
    let home = tempfile::tempdir().unwrap();
    let socket = home.path().join("hook.sock");
    let cli = env!("CARGO_BIN_EXE_limux-cli");
    let output = private_command(cli, home.path(), &socket)
        .args(["hooks", "setup", "claude"])
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    let settings: Value =
        serde_json::from_slice(&std::fs::read(home.path().join(".claude/settings.json")).unwrap())
            .unwrap();
    for (event, title, body) in [
        ("Notification", "Claude needs you", "waiting for input"),
        ("Stop", "Claude finished", "task complete"),
    ] {
        let hook = settings["hooks"][event][0]["hooks"][0]["command"]
            .as_str()
            .unwrap();
        let mut command = terminal_command("/bin/sh", home.path(), &socket);
        command.args(["-c", hook]);
        let params = notification_params(command, &socket, json!({"hook_event_name": event}));
        assert_eq!(params["title"], title, "installed {event} hook");
        assert_eq!(params["body"], body, "installed {event} hook");
    }
}

#[test]
fn legacy_claude_stop_route_respects_notification_payload_and_explicit_event() {
    let home = tempfile::tempdir().unwrap();
    let socket = home.path().join("hook.sock");
    let cli = env!("CARGO_BIN_EXE_limux-cli");
    for (args, event, title) in [
        (vec!["stop"], "Notification", "Claude needs you"),
        (vec!["stop"], "Stop", "Claude finished"),
        (vec!["--event", "stop"], "Notification", "Claude finished"),
    ] {
        let mut command = terminal_command(cli, home.path(), &socket);
        command.args(["--json", "hooks", "claude"]).args(args);
        let params = notification_params(command, &socket, json!({"hook_event_name": event}));
        assert_eq!(params["title"], title);
    }
}

// An agent outside every Limux terminal (another terminal, an IDE, T3 Code)
// still runs the global hooks and finds the default socket. Its note must not
// land on whatever project is current, where nothing would ever clear it.
#[test]
fn a_hook_outside_limux_sends_no_notification() {
    let home = tempfile::tempdir().unwrap();
    let socket = home.path().join("hook.sock");
    let cli = env!("CARGO_BIN_EXE_limux-cli");
    let listener = UnixListener::bind(&socket).unwrap();
    listener.set_nonblocking(true).unwrap();
    for event in ["Stop", "Notification"] {
        let mut child = private_command(cli, home.path(), &socket)
            .args(["--json", "hooks", "claude", "stop"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(
                json!({"hook_event_name": event, "last_assistant_message": "I opened PR #21"})
                    .to_string()
                    .as_bytes(),
            )
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while child.try_wait().unwrap().is_none() {
            if let Ok((stream, _)) = listener.accept() {
                let _ = child.kill();
                drop(stream);
                panic!("{event} hook outside Limux reached the socket");
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                panic!("{event} hook did not exit");
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
        let reply: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(reply["continue"], true, "{event} hook still answers Claude");
    }
}

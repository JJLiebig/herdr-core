//! Terminal cleanup on a normal end or after losing a holder.

#![cfg(unix)] // Windows: tests/windows.rs

mod common;

use common::*;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

/// A Python process that ignores hangup (and with `term`, terminate too),
/// writing its pid to `file`; with `own_session`, it leaves the terminal's
/// session first, like agents' tool shells. Python, not perl or sh: macOS
/// hides the environment of its own binaries, ours or not.
fn python(s: &Session, file: &str, own_session: bool, term: bool) -> String {
    let setsid = if own_session { "os.setsid(); " } else { "" };
    let ignore_term = if term { "signal.signal(signal.SIGTERM, signal.SIG_IGN); " } else { "" };
    format!(
        "python3 -c 'import os, signal, time; {setsid}signal.signal(signal.SIGHUP, signal.SIG_IGN); {ignore_term}open(\"{}\", \"w\").write(str(os.getpid())); time.sleep(300)'",
        s.root.join(file).display()
    )
}

fn stubborn(s: &Session, file: &str, own_session: bool) -> String {
    format!("{} &", python(s, file, own_session, true))
}

fn pid_in(s: &Session, file: &str) -> Option<i64> {
    std::fs::read_to_string(s.root.join(file)).ok()?.trim().parse().ok()
}

/// Alive and not a zombie.
fn running(pid: i64) -> bool {
    match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => !stat.rsplit(')').next().unwrap_or("").trim_start().starts_with('Z'),
        Err(_) => cfg!(target_os = "macos") && pid_alive(pid),
    }
}

fn started(s: &Session, files: &[&str]) -> Vec<i64> {
    wait_until("helpers started", || files.iter().all(|f| pid_in(s, f).is_some()));
    files.iter().map(|f| pid_in(s, f).unwrap()).collect()
}

fn gone(pids: &[i64]) {
    wait_until("every process it started ended", || !pids.iter().any(|p| running(*p)));
}

fn run(s: &Session, script: &str) -> String {
    s.json(&["run", "--bg", "sh", "-c", script])["address"].as_str().unwrap().to_string()
}

struct Survivors(Vec<i64>);

impl Drop for Survivors {
    fn drop(&mut self) {
        for &pid in &self.0 {
            unsafe { libc::kill(pid as i32, libc::SIGKILL) };
        }
        gone(&self.0);
    }
}

/// The detached child echoes on its socket until it is killed or the test drops the connection.
fn with_detached_child(s: &Session, name: &str, start: impl FnOnce(&str) -> String) -> (String, UnixStream) {
    let path = s.root.join(name);
    let listener = UnixListener::bind(&path).unwrap();
    listener.set_nonblocking(true).unwrap();
    let child = r#"
import os, signal, socket, sys
os.setsid()
signal.signal(signal.SIGHUP, signal.SIG_IGN)
signal.signal(signal.SIGTERM, signal.SIG_IGN)
with socket.socket(socket.AF_UNIX) as conn:
    conn.connect(sys.argv[1])
    while data := conn.recv(1):
        conn.sendall(data)
"#;
    let terminal = start(&format!("python3 -c '{child}' '{}' &", path.display()));
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let connected = runtime.block_on(async {
        let listener = tokio::net::UnixListener::from_std(listener).unwrap();
        tokio::time::timeout(Duration::from_secs(15), listener.accept()).await
    });
    let (conn, _) =
        connected.unwrap_or_else(|_| panic!("detached child did not connect:\n{}", text(s, &terminal))).unwrap();
    let conn = conn.into_std().unwrap();
    conn.set_nonblocking(false).unwrap();
    conn.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    (terminal, conn)
}

fn child_ended(mut conn: UnixStream) {
    assert_eq!(conn.read(&mut [0]).expect("the detached child should close its socket within the sweep window"), 0);
}

fn child_still_running(mut conn: &UnixStream) {
    conn.write_all(b"x").unwrap();
    let mut echo = [0];
    conn.read_exact(&mut echo).expect("the detached child should still respond");
    assert_eq!(&echo, b"x");
}

#[test]
fn kill_wakes_stopped_foreground_job_after_foreground_poll() {
    let mut s = Session::new();
    s.env.push(("HC_FOREGROUND_MS".into(), "10".into()));
    let helper = s.root.join("stopped_fg_poll.py");
    let pid_file = s.root.join("stopped-fg-poll");
    // Mimic an interactive shell whose foreground job (in its own process group)
    // catches SIGHUP and stops before the shell exits after reclaiming the PTY.
    // Double-forking orphans the job's process group up front so the shell's
    // later exit does not trigger the kernel's orphaned-pgrp SIGHUP+SIGCONT,
    // and SIGSTOP stops the job deterministically before teardown runs.
    std::fs::write(
        &helper,
        r#"import os, signal, sys, time

pid_file = sys.argv[1]
r_fd, w_fd = os.pipe()
c_r, c_w = os.pipe()
mid = os.fork()
if mid == 0:
    os.close(r_fd)
    os.close(w_fd)
    child = os.fork()
    if child == 0:
        os.close(c_r)
        os.setpgid(0, 0)
        got_hup = []
        signal.signal(signal.SIGHUP, lambda *_: got_hup.append(1))
        os.write(c_w, str(os.getpid()).encode())
        os.close(c_w)
        while not got_hup:
            time.sleep(0.01)
        os.kill(os.getpid(), signal.SIGSTOP)
        os._exit(0)
    os.close(c_r)
    os.close(c_w)
    os._exit(0)
os.close(c_w)
os.waitpid(mid, 0)
child = int(os.read(c_r, 32).decode())
os.close(c_r)
signal.signal(signal.SIGTTOU, signal.SIG_IGN)
try:
    os.setpgid(child, child)
except OSError:
    pass
os.tcsetpgrp(0, child)
def on_hup(*_):
    try:
        os.tcsetpgrp(0, os.getpid())
    except OSError:
        pass
    time.sleep(0.1)
    os._exit(0)
signal.signal(signal.SIGHUP, on_hup)
with open(pid_file, "w") as f:
    f.write(str(child))
os.read(r_fd, 1)
"#,
    )
    .unwrap();
    // Exercise the holder signal path independently of the fork's later HC_SCOPE sweep.
    let t = run(&s, &format!("exec env -u HC_SCOPE python3 {helper:?} {pid_file:?}"));
    let pids = started(&s, &["stopped-fg-poll"]);
    let _cleanup = Survivors(pids.clone());
    s.json(&["kill", &t]);
    gone(&pids);
}

#[test]
fn kill_escalation_spares_former_foreground_job_ignoring_hangup() {
    let mut s = Session::new();
    s.env.push(("HC_FOREGROUND_MS".into(), "60000".into()));
    let helper = s.root.join("turnover_fg.py");
    let pid_file = s.root.join("turnover-fg");
    std::fs::write(
        &helper,
        r#"import os, signal, sys, time

pid_file = sys.argv[1]
r_fd, w_fd = os.pipe()
child = os.fork()
if child == 0:
    os.close(w_fd)
    os.setpgid(0, 0)
    os.read(r_fd, 1)
    os.close(r_fd)
    signal.signal(signal.SIGHUP, signal.SIG_IGN)
    with open(pid_file, "w") as f:
        f.write(str(os.getpid()))
    time.sleep(300)
else:
    os.close(r_fd)
    signal.signal(signal.SIGTTOU, signal.SIG_IGN)
    try:
        os.setpgid(child, child)
    except OSError:
        pass
    os.tcsetpgrp(0, child)
    os.close(w_fd)
    def on_hup(*_):
        try:
            os.tcsetpgrp(0, os.getpid())
        except OSError:
            pass
        time.sleep(300)
    signal.signal(signal.SIGHUP, on_hup)
    os.waitpid(child, 0)
"#,
    )
    .unwrap();
    // Exercise the holder signal path independently of the fork's later HC_SCOPE sweep.
    let t = run(&s, &format!("exec env -u HC_SCOPE python3 {helper:?} {pid_file:?}"));
    let pids = started(&s, &["turnover-fg"]);
    let survivors = Survivors(pids);
    s.json(&["kill", &t]);
    assert!(survivors.0.iter().all(|p| running(*p)), "escalation killed a former foreground job that ignores hangup");
}

#[test]
fn normal_ends_sweep_only_the_ended_terminals() {
    let s = Session::new();
    let start = |child: &str| run(&s, &format!("{child} read -r go"));
    let (killed, killed_child) = with_detached_child(&s, "killed.sock", start);
    let (exiting, exiting_child) = with_detached_child(&s, "exiting.sock", start);

    assert_eq!(s.json(&["kill", &killed])["reason"], "killed");
    child_ended(killed_child);
    child_still_running(&exiting_child);

    s.json(&["keys", &exiting, "enter"]);
    assert_eq!(s.json(&["wait", &exiting, "--timeout", "5"])["exit_code"], 0);
    child_ended(exiting_child);
}

#[test]
fn server_stop_sweeps_the_ended_program_of_a_kept_terminal() {
    let s = Session::new();
    let (kept, child) = with_detached_child(&s, "kept.sock", |child| {
        s.json(&["run", "--bg", "--keep", "sh", "-c", &format!("{child} read -r go")])["address"]
            .as_str()
            .unwrap()
            .to_string()
    });
    s.json(&["server", "stop"]);
    child_ended(child);
    assert_eq!(get(&s, &kept)["state"], "live");
}

#[test]
#[cfg(target_os = "linux")]
fn closing_detached_terminals_releases_the_holders_pty_masters() {
    let s = Session::new();
    let warm = run(&s, "true");
    wait_until("warm terminal ended", || s.json(&["get", &warm])["state"] == "finished");
    let holder = s.json(&["server", "status"])["holder"]["pid"].as_u64().unwrap();
    let masters = || {
        std::fs::read_dir(format!("/proc/{holder}/fd"))
            .unwrap()
            .flatten()
            .filter_map(|fd| std::fs::read_link(fd.path()).ok())
            .filter(|path| path.ends_with("ptmx"))
            .count()
    };
    wait_until("warm PTY released", || masters() == 0);
    eprintln!("holder {holder}: before closing detached terminals, {} PTY masters", masters());
    let mut survivors = Vec::new();
    for i in 0..3 {
        // The runtime marker keeps the child (and its open PTY) alive after the sweep.
        let (t, child) = with_detached_child(&s, &format!("detached-{i}.sock"), |child| {
            run(&s, &format!("HC_RUNTIME=1 {child} read -r go"))
        });
        s.json(&["kill", &t]);
        survivors.push(child);
    }
    wait_until("closed terminals released every PTY master", || masters() == 0);
    eprintln!("holder {holder}: after closing three detached terminals, {} PTY masters", masters());
    s.json(&["server", "stop"]); // waits for every sweep before checking the protected processes
    for child in &survivors {
        child_still_running(child);
    }
}

#[test]
fn a_replacement_whose_holder_was_lost_is_recovered_and_cleaned_up() {
    let s = Session::new();
    let t = s.json(&["run", "--bg", "--keep", "sleep", "300"])["address"].as_str().unwrap().to_string();
    s.json(&["server", "stop"]);
    // What a crash leaves: a holder had claimed the replacement program but
    // never reported it started, and something it launched is still running.
    let path = s.root.join("state/herdr-core/test/terminals.json");
    let mut reg: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let token = format!("0-replacement-{}", std::process::id());
    let rec = reg["terminals"].as_object_mut().unwrap().values_mut().next().unwrap();
    rec["pid"] = serde_json::Value::Null;
    rec["reason"] = "server_stop".into();
    rec["holder"] = "a-holder-that-is-gone".into();
    rec["scope"] = token.clone().into();
    std::fs::write(&path, serde_json::to_vec(&reg).unwrap()).unwrap();
    let launched = std::process::Command::new("sh")
        .args(["-c", &format!("{} &", python(&s, "left", false, false))])
        .env("HC_SCOPE", &token)
        .status()
        .unwrap();
    assert!(launched.success());
    let left = started(&s, &["left"]);
    s.json(&["server", "status"]);
    gone(&left);
    wait_until("the kept terminal got a new shell", || get(&s, &t)["pid"].is_number());
}

#[test]
fn a_lost_holder_takes_its_programs_with_it() {
    let s = Session::new();
    let t = run(&s, &format!("{} exec {}", stubborn(&s, "detached", true), python(&s, "leader", false, false)));
    let pids = started(&s, &["leader", "detached"]);
    let holder = s.json(&["server", "status"])["holder"]["pid"].as_i64().unwrap();
    unsafe { libc::kill(holder as i32, libc::SIGKILL) };
    wait_until("holder lost", || s.json(&["get", &t])["reason"] == "holder_lost");
    gone(&pids);
}

#[test]
fn a_stop_right_after_a_lost_holder_still_ends_its_programs() {
    let s = Session::new();
    run(&s, &format!("{} exec {}", stubborn(&s, "detached", true), python(&s, "leader", false, true)));
    let pids = started(&s, &["leader", "detached"]);
    let holder = s.json(&["server", "status"])["holder"]["pid"].as_i64().unwrap();
    unsafe { libc::kill(holder as i32, libc::SIGKILL) };
    wait_until("holder lost", || s.json(&["server", "status"])["holders"].as_array().unwrap().is_empty());
    s.json(&["server", "stop"]);
    assert!(!pids.iter().any(|p| running(*p)), "stop returned with the lost holder's programs alive");
}

#[test]
fn a_holder_dying_during_stop_still_has_its_programs_ended_before_stop_returns() {
    let s = Session::new();
    run(&s, &format!("{} sleep 300", stubborn(&s, "detached", true)));
    let pids = started(&s, &["detached"]);
    let holder = s.json(&["server", "status"])["holder"]["pid"].as_i64().unwrap() as i32;
    unsafe { libc::kill(holder, libc::SIGSTOP) }; // frozen: stop gets past killing into its goodbye
    let stop = s.cmd().args(["server", "stop"]).spawn().unwrap();
    std::thread::sleep(Duration::from_secs(4));
    unsafe { libc::kill(holder, libc::SIGKILL) };
    let out = stop.wait_with_output().unwrap();
    assert!(out.status.success());
    assert!(!pids.iter().any(|p| running(*p)), "stop returned with the dead holder's programs alive");
}

#[test]
fn a_terminal_that_started_another_hc_session_does_not_take_it_down() {
    let s = Session::new();
    let inner = Session::new();
    let (t, child) = with_detached_child(&s, "inner.sock", |child| {
        let root = inner.root.display();
        run(
            &s,
            &format!(
                "HOME='{root}' XDG_RUNTIME_DIR='{root}/run' XDG_STATE_HOME='{root}/state' \
                 XDG_CONFIG_HOME='{root}/config' '{}' server start || exit 1; {child} read -r go",
                inner.bin.display()
            ),
        )
    });
    let pid = || inner.connect().call(1, "server.status", serde_json::json!({}))["result"]["pid"].as_u64().unwrap();
    let started = pid();
    s.json(&["kill", &t]);
    child_ended(child);
    assert_eq!(pid(), started, "the other session's core is still running");
}

#[test]
fn a_run_while_stopping_is_refused() {
    let s = Session::new();
    // Ends only on KILL, so the stop takes a moment.
    let t = run(&s, "trap '' HUP TERM; while :; do sleep 1; done");
    wait_until("running", || s.json(&["get", &t])["pid"].is_number());
    let stop = {
        let mut cmd = s.cmd();
        cmd.args(["server", "stop"]);
        std::thread::spawn(move || cmd.output().unwrap())
    };
    std::thread::sleep(Duration::from_millis(300));
    let (_, err) = s.err(&["run", "--bg", "sleep", "300"]);
    assert_eq!(err["error"]["kind"], "unavailable", "{err}");
    assert!(stop.join().unwrap().status.success());
}

#[test]
fn a_program_that_starts_after_its_run_gave_up_is_ended() {
    let mut s = Session::new();
    s.env.push(("HC_SPAWN_MS".into(), "500".into()));
    s.json(&["run", "--bg", "true"]); // a holder now runs
    let holder = s.json(&["server", "status"])["holder"]["pid"].as_i64().unwrap();
    unsafe { libc::kill(holder as i32, libc::SIGSTOP) };
    let (_, err) =
        s.err(&["run", "--bg", "sh", "-c", &format!("echo $$ > {}; sleep 300", s.root.join("late").display())]);
    unsafe { libc::kill(holder as i32, libc::SIGCONT) };
    assert_eq!(err["error"]["kind"], "spawn_failed", "{err}");
    // It may die before it even writes its pid; either way it must not live on.
    std::thread::sleep(Duration::from_millis(1000));
    if let Some(pid) = pid_in(&s, "late") {
        gone(&[pid]);
    }
}

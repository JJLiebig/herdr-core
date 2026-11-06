use super::*;

#[test]
fn restart_and_a_core_crash_keep_terminals_and_stop_leaves_nothing() {
    let s = Session::new();
    s.json(&["server", "start"]);
    let t = s.bg(&["ping", "-n", "120", "127.0.0.1"]);
    let program = s.json(&["get", &t])["pid"].as_u64().unwrap();
    let status = s.json(&["server", "status"]);
    let holder = status["holder"]["pid"].as_u64().unwrap();

    s.json(&["server", "restart"]);
    assert_eq!(s.json(&["get", &t])["state"], "live");

    let core = s.json(&["server", "status"])["pid"].as_u64().unwrap();
    Command::new("taskkill").args(["/F", "/PID", &core.to_string()]).output().unwrap();
    wait_until("core back", || !alive(core) && s.out(&["server", "status"]).0 == 0);
    let after = s.json(&["server", "status"]);
    assert_eq!(after["holder"]["pid"].as_u64(), Some(holder), "same holder: {after}");
    assert_eq!(s.json(&["get", &t])["state"], "live");

    let core = after["pid"].as_u64().unwrap();
    s.json(&["server", "stop"]);
    wait_until("nothing left", || ![program, holder, core].into_iter().any(alive));
}

#[test]
fn the_folder_a_shell_moves_to_is_seen_and_a_lost_kept_shell_returns_there() {
    let s = Session::new();
    s.json(&["server", "start"]);
    let dir = s.root.join("moved here");
    std::fs::create_dir(&dir).unwrap();
    let dir = dir.display().to_string();
    // TEMP may use an 8.3 name; PowerShell expands it to the same folder's long name.
    let expected = std::fs::canonicalize(&dir).unwrap();
    let cwd =
        |t: &str| std::fs::canonicalize(s.json(&["get", t])["foreground"]["cwd"].as_str().unwrap_or_default()).ok();

    let cmd = s.bg(&["cmd.exe"]);
    s.out(&["send", &cmd, &format!("cd /d \"{dir}\"")]);
    wait_until("cmd's folder", || cwd(&cmd).as_ref() == Some(&expected));

    // PowerShell keeps its location to itself: hc's own shell tells Windows.
    let shell = s.bg(&["--keep"]);
    s.out(&["send", &shell, &format!("Set-Location '{dir}'")]);
    wait_until("PowerShell's folder", || cwd(&shell).as_ref() == Some(&expected));

    let old = s.json(&["get", &shell])["pid"].as_u64().unwrap();
    let holder = s.json(&["server", "status"])["holder"]["pid"].as_u64().unwrap();
    Command::new("taskkill").args(["/F", "/PID", &holder.to_string()]).output().unwrap();
    wait_until("replacement shell", || {
        let r = s.json(&["get", &shell]);
        r["state"] == "live" && r["pid"].as_u64().is_some_and(|p| p != old)
    });
    assert_eq!(std::fs::canonicalize(s.json(&["get", &shell])["cwd"].as_str().unwrap()).unwrap(), expected);
}

#[test]
fn explicit_recovery_uses_one_same_id_command_and_adopts_surviving_holders() {
    use std::io::{BufRead, Write};
    let s = Session::new();
    s.json(&["server", "start"]);
    let t = s.bg(&["--keep", "ping", "-n", "120", "127.0.0.1"]);
    let dir = s.root.join("fixed recovery cwd");
    std::fs::create_dir(&dir).unwrap();
    let marker = dir.join("starts.txt");
    let script = s.file(
        "recover.ps1",
        "Set-Content scope.txt $env:HC_SCOPE\nAdd-Content starts.txt 'start'\nWrite-Output 'RECOVERED-STATIC'\nStart-Sleep 100\n",
    );
    let state: Value = serde_json::from_slice(
        &std::fs::read(s.root.join("state/herdr-core").join(&s.name).join("terminals.json")).unwrap(),
    )
    .unwrap();
    let scope = state["terminals"]["1"]["scope"].as_str().unwrap();
    let request = serde_json::json!({"jsonrpc":"2.0", "id":1, "method":"terminal.keep", "params": {
        "terminal":t, "keep":true, "expected_scope":scope,
        "recovery":{"argv":["powershell", "-NoProfile", "-File", script], "cwd":dir.display().to_string()}
    }});
    let mut conn = cmd(&s.name, &s.root)
        .arg("connect")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = conn.stdin.take().unwrap();
    writeln!(input, "{request}").unwrap();
    let mut reply = String::new();
    std::io::BufReader::new(conn.stdout.take().unwrap()).read_line(&mut reply).unwrap();
    drop(input);
    conn.wait().unwrap();
    let reply: Value = serde_json::from_str(&reply).unwrap();
    assert!(reply["result"]["keep"].as_bool().unwrap_or(false), "{reply}");
    assert!(reply["result"].get("recovery").is_none());
    let old = s.json(&["get", &t])["pid"].as_u64().unwrap();
    s.json(&["server", "restart"]);
    assert_eq!(s.json(&["get", &t])["pid"], old, "planned handover adopts the holder");
    assert!(!marker.exists());
    let status = s.json(&["server", "status"]);
    for pid in [status["pid"].as_u64().unwrap(), status["holder"]["pid"].as_u64().unwrap()] {
        Command::new("taskkill").args(["/F", "/PID", &pid.to_string()]).output().unwrap();
    }
    s.json(&["server", "start"]);
    wait_until("recorded command recovers after core and holder loss", || {
        let r = s.json(&["get", &t]);
        r["state"] == "live" && r["pid"].as_u64().is_some_and(|p| p != old) && marker.exists()
    });
    s.shows(&t, "RECOVERED-STATIC");
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1);
    assert_eq!(s.json(&["get", &t])["cwd"], dir.display().to_string());
    assert_eq!(s.json(&["ls"])["terminals"].as_array().unwrap().len(), 1);
    let state_dir = s.root.join("state/herdr-core").join(&s.name);
    let holder = s.json(&["server", "status"])["holder"]["pid"].as_u64().unwrap();
    let blocked = state_dir.join("terminals.tmp");
    std::fs::create_dir(&blocked).unwrap();
    Command::new("taskkill").args(["/F", "/PID", &holder.to_string()]).output().unwrap();
    wait_until("recovery claim write fails", || {
        std::fs::read_to_string(state_dir.join("core.log")).is_ok_and(|log| log.contains("cannot save recovery claim"))
    });
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1, "failed claim must not spawn");
    std::fs::remove_dir(&blocked).unwrap();
    wait_until("recovery waits for a durable claim", || {
        std::fs::read_to_string(&marker).is_ok_and(|starts| starts.lines().count() == 2)
    });
    let state: Value = serde_json::from_slice(&std::fs::read(state_dir.join("terminals.json")).unwrap()).unwrap();
    assert_eq!(
        state["terminals"]["1"]["scope"].as_str().unwrap(),
        std::fs::read_to_string(dir.join("scope.txt")).unwrap().trim()
    );
    s.json(&["server", "restart"]);
    assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 2, "no duplicate on adoption");
    assert_eq!(s.json(&["kill", &t])["reason"], "killed");
    assert_eq!(s.json(&["get", &t])["state"], "finished");
}

#[test]
fn configured_shell_uses_argv_and_is_read_live_for_new_and_recovered_terminals() {
    let s = Session::new();
    let config = s.root.join("config/herdr-core/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    let exe = s.root.join("shell with spaces.exe");
    std::fs::copy(std::env::var("COMSPEC").unwrap(), &exe).unwrap();
    let set_shell = |marker: &str| {
        let argv = serde_json::json!([exe, "/d", "/k", format!("echo {marker}")]);
        std::fs::write(&config, format!("shell = {argv}\n")).unwrap();
    };
    s.json(&["server", "start"]);
    set_shell("configured-first");
    let t = s.bg(&["--keep"]);
    s.shows(&t, "configured-first");
    let explicit = s.bg(&["cmd.exe", "/d", "/k", "echo explicit-program"]);
    s.shows(&explicit, "explicit-program");
    assert!(!s.read(&explicit).contains("configured-first"));
    let old = s.json(&["get", &t])["pid"].as_u64().unwrap();
    set_shell("configured-recovery");
    s.json(&["server", "stop"]);
    s.json(&["server", "start"]);
    wait_until("configured shell restored", || s.json(&["get", &t])["pid"].as_u64().is_some_and(|p| p != old));
    s.shows(&t, "configured-recovery");
    let pid = s.json(&["get", &t])["pid"].as_u64().unwrap();
    let holder = s.json(&["server", "status"])["holder"]["pid"].as_u64().unwrap();
    Command::new("taskkill").args(["/F", "/PID", &holder.to_string()]).output().unwrap();
    wait_until("configured keep replacement", || s.json(&["get", &t])["pid"].as_u64().is_some_and(|p| p != pid));
    s.shows(&t, "configured-recovery");
    for (value, kind) in [("[]", "config"), (r#"["hc-missing-shell-24"]"#, "spawn_failed")] {
        std::fs::write(&config, format!("shell = {value}\n")).unwrap();
        let out = cmd(&s.name, &s.root).args(["run", "--bg"]).output().unwrap();
        assert!(!out.status.success());
        let err: Value = serde_json::from_slice(&out.stderr).unwrap();
        assert_eq!(err["error"]["kind"], kind, "{err}");
    }
}

#[test]
fn unavailable_explicit_recovery_starts_a_shell_and_can_resume_after_restoration() {
    use std::io::{BufRead, Write};
    for missing in ["folder", "executable", "shell_executable"] {
        let s = Session::new();
        s.json(&["server", "start"]);
        let config = s.root.join("config/herdr-core/config.toml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "shell = ['cmd.exe', '/d', '/k', 'echo FALLBACK-SHELL']\n").unwrap();
        if missing == "shell_executable" {
            let broken = s.file("invalid shell.exe", "not an executable");
            std::fs::write(&config, format!("shell = {}\n", json!([broken]))).unwrap();
        }
        let t = s.bg(&["--keep", "ping", "-n", "120", "127.0.0.1"]);
        wait_until("original folder observed", || {
            s.json(&["get", &t])["foreground"]["cwd"] == s.root.display().to_string()
        });
        let dir = s.root.join("agent's folder & data");
        std::fs::create_dir(&dir).unwrap();
        let exe = s.root.join("resume agent.exe");
        std::fs::copy(std::env::var("COMSPEC").unwrap(), &exe).unwrap();
        let state_path = s.root.join("state/herdr-core").join(&s.name).join("terminals.json");
        let state: Value = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        let command = if missing == "folder" {
            format!("echo AGENT-RESUMED {}", "x".repeat(6144))
        } else {
            "echo AGENT-RESUMED".into()
        };
        let recovery = json!({"argv":[exe, "/d", "/k", command], "cwd":dir});
        let request = json!({"jsonrpc":"2.0", "id":1, "method":"terminal.keep", "params": {
            "terminal":t, "keep":true, "expected_scope":state["terminals"]["1"]["scope"], "recovery":recovery
        }});
        let mut conn = cmd(&s.name, &s.root)
            .arg("connect")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let mut input = conn.stdin.take().unwrap();
        writeln!(input, "{request}").unwrap();
        let mut reply = String::new();
        std::io::BufReader::new(conn.stdout.take().unwrap()).read_line(&mut reply).unwrap();
        drop(input);
        conn.wait().unwrap();
        assert!(serde_json::from_str::<Value>(&reply).unwrap()["result"]["keep"] == true, "{reply}");
        let status = s.json(&["server", "status"]);
        for pid in [status["pid"].as_u64().unwrap(), status["holder"]["pid"].as_u64().unwrap()] {
            Command::new("taskkill").args(["/F", "/PID", &pid.to_string()]).output().unwrap();
        }
        if missing != "executable" {
            std::fs::remove_dir(&dir).unwrap();
        } else {
            std::fs::remove_file(&exe).unwrap();
        }
        s.json(&["server", "start"]);
        if missing == "shell_executable" {
            wait_until("failed shell exec remains live and retries", || {
                let state: Value = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
                s.json(&["get", &t])["state"] == "live"
                    && state["terminals"]["1"]["restart_delay_ms"].as_u64().is_some_and(|n| n >= 3000)
            });
            assert!(s.json(&["get", &t])["finished_ms"].is_null());
            std::fs::write(&config, "shell = ['cmd.exe', '/d', '/k', 'echo FALLBACK-SHELL']\n").unwrap();
        }
        s.shows(&t, "FALLBACK-SHELL");
        s.shows(&t, "Resume:");
        let screen = s.read(&t);
        assert!(screen.contains("AGENT-RESUMED"), "{screen}");
        assert!(
            screen.contains(if missing != "executable" { "folder is gone" } else { "could not start" }),
            "{screen}"
        );
        assert!(screen.find("Resume:") < screen.find("FALLBACK-SHELL"), "notice precedes shell output: {screen}");
        assert_eq!(s.json(&["get", &t])["cwd"], s.root.display().to_string());
        s.typed(&t, "echo shell-is-usable");
        s.shows(&t, "shell-is-usable");
        let state: Value = serde_json::from_slice(&std::fs::read(&state_path).unwrap()).unwrap();
        assert_eq!(state["terminals"]["1"]["recovery"], recovery);
        if missing != "executable" {
            std::fs::create_dir(&dir).unwrap();
        } else {
            std::fs::copy(std::env::var("COMSPEC").unwrap(), &exe).unwrap();
        }
        let holder = s.json(&["server", "status"])["holder"]["pid"].as_u64().unwrap();
        Command::new("taskkill").args(["/F", "/PID", &holder.to_string()]).output().unwrap();
        wait_until("restored recovery resumes", || s.json(&["get", &t])["cwd"] == dir.display().to_string());
        s.shows(&t, "AGENT-RESUMED");
        assert!(!s.read(&t).contains("Resume:"));
    }
}

#[test]
fn osc_resume_runs_inside_the_recovered_shell_and_a_failed_command_leaves_it_usable() {
    let s = Session::new();
    s.json(&["server", "start"]);
    let config = s.root.join("config/herdr-core/config.toml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(
        &config,
        r#"shell = ['cmd.exe', '/d']
resume_commands = { probe = 'cmd /d /c "echo {session}>resume-result.txt & exit 7"' }
"#,
    )
    .unwrap();
    let script = s.file("agent.ps1", "$e=[char]27; Write-Host -NoNewline \"$e]7501;state=idle:app=probe:session=01234567-89ab-cdef-0123-456789abcdef$e\\\"; Start-Sleep 120");
    let t = s.bg(&["--keep", "--cwd", s.root.to_str().unwrap(), "powershell", "-NoProfile", "-File", &script]);
    wait_until("OSC binding saved", || {
        s.json(&["get", &t])["resume_session"] == "01234567-89ab-cdef-0123-456789abcdef"
    });
    let holder = s.json(&["server", "status"])["holder"]["pid"].as_u64().unwrap();
    Command::new("taskkill").args(["/F", "/PID", &holder.to_string()]).output().unwrap();
    let result = s.root.join("resume-result.txt");
    wait_until("resume command executed", || result.is_file());
    assert_eq!(std::fs::read_to_string(result).unwrap().trim(), "01234567-89ab-cdef-0123-456789abcdef");
    let terminal = s.json(&["get", &t]);
    assert_eq!(terminal["state"], "live");
    assert_eq!(terminal["argv"][0], "cmd.exe");
    s.json(&["send", &t, "echo usable>shell-result.txt"]);
    wait_until("shell accepts input after failed resume", || s.root.join("shell-result.txt").is_file());
}

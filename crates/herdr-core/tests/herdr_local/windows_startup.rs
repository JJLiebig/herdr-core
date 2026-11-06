use super::*;

#[test]
fn local_and_ssh_clients_use_an_existing_core_instead_of_starting_another() {
    let s = Session::new();
    let first = s.json(&["server", "start"]);
    for ssh in [false, true] {
        let mut command = cmd(&s.name, &s.root);
        if ssh {
            command.env("SSH_CLIENT", "10.0.0.1 50000 22");
        }
        let out = command.args(["ls"]).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(s.json(&["server", "status"])["pid"], first["pid"]);
    }
}

#[test]
fn implicit_local_and_ssh_start_apply_the_same_desktop_policy() {
    let mut outcomes = Vec::new();
    for ssh in [false, true] {
        let s = Session::new();
        let mut command = cmd(&s.name, &s.root);
        if ssh {
            command.env("SSH_CLIENT", "10.0.0.1 50000 22");
        }
        let out = command.args(["ls"]).output().unwrap();
        outcomes.push(out.status.success());
        if out.status.success() {
            assert!(s.json(&["server", "status"])["pid"].is_number());
        } else {
            let error: Value = serde_json::from_slice(&out.stderr).unwrap();
            assert!(error["error"]["message"].as_str().unwrap().contains("desktop session"), "{error}");
            assert!(error["error"]["hint"].as_str().unwrap().contains("open Herdr on that Windows machine"), "{error}");
            assert_ne!(s.out(&["server", "status"]).0, 0, "refused startup left a core");
        }
        if !out.status.success() {
            let exe = installed(&s);
            let new = release(&s.root.join("new"), "new-build");
            let mut update = cmd_at(&exe, &s.name, &s.root);
            if ssh {
                update.env("SSH_CLIENT", "10.0.0.1 50000 22");
            }
            let refused = update.args(["update", "--from", new.to_str().unwrap()]).output().unwrap();
            assert!(!refused.status.success());
            let error: Value = serde_json::from_slice(&refused.stderr).unwrap();
            assert!(error["error"]["message"].as_str().unwrap().contains("desktop session"), "{error}");
            assert!(marked(&exe, "old-build"), "refused update replaced the installed core");
            assert!(!marked(&exe, "new-build"));
            assert_ne!(s.out(&["server", "status"]).0, 0, "refused update left a core");
        }
        if out.status.success() {
            assert_eq!(s.out(&["server", "stop"]).0, 0);
        }
        // Explicit startup must work even when implicit startup was refused.
        let mut explicit = cmd(&s.name, &s.root);
        if ssh {
            explicit.env("SSH_CLIENT", "10.0.0.1 50000 22");
        }
        let out = explicit.args(["server", "start"]).output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    }
    assert_eq!(outcomes[0], outcomes[1], "SSH variables must not change desktop startup policy");
}

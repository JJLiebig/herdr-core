//! Private fork: explicit recovery and live OSC session recovery on the kept lifecycle.
use crate::api::KeepParams;
use crate::holder_proto as proto;
use crate::rpc::RpcError;
use crate::runtime::{Core, State};
use crate::terminals::{Public, Terminal};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::atomic::Ordering;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Recovery {
    pub argv: Vec<String>,
    pub cwd: String,
}

fn osc_trim(value: &[u8]) -> &[u8] {
    let whitespace = b" \t\n\r\x0b\x0c";
    let start = value.iter().position(|c| !whitespace.contains(c)).unwrap_or(value.len());
    let end = value.iter().rposition(|c| !whitespace.contains(c)).map_or(start, |i| i + 1);
    &value[start..end]
}

fn osc_field<'a>(raw: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    raw.split(|c| *c == b':')
        .filter_map(|field| {
            let equals = field.iter().position(|c| *c == b'=')?;
            let (field_key, value) = field.split_at(equals);
            (osc_trim(field_key) == key).then(|| osc_trim(&value[1..]))
        })
        .next_back()
        .filter(|s| !s.is_empty())
}

fn osc_value<'a>(raw: &'a [u8], key: &[u8]) -> Option<&'a [u8]> {
    osc_field(raw, key).filter(|v| v.iter().all(|c| c.is_ascii_alphanumeric() || b"_.+-,/=".contains(c)))
}

fn valid_session(value: &[u8]) -> bool {
    value.len() == 36
        && value.iter().enumerate().all(
            |(i, c)| {
                if [8, 13, 18, 23].contains(&i) { *c == b'-' } else { c.is_ascii_hexdigit() }
            },
        )
}

impl Core {
    /// New live output only: replaying a tail could resurrect a deliberately reset session.
    pub(crate) fn bind_resume_session(&self, st: &mut State, id: u32, report: &ghostty_vt::StatusReport) {
        if !report.id.is_empty() || osc_field(&report.raw, b"id").is_some() {
            return;
        }
        let Some(t) = st.reg.terminals.get_mut(&id).filter(|t| t.is_live() && t.keep && t.holder.is_some()) else {
            return;
        };
        let binding = if report.state == "clear"
            || osc_field(&report.raw, b"app").is_none()
            || osc_field(&report.raw, b"session").is_none()
        {
            (None, None)
        } else {
            let Some(app) = osc_value(&report.raw, b"app").and_then(|v| std::str::from_utf8(v).ok()) else { return };
            let Some(session) = osc_field(&report.raw, b"session").filter(|v| valid_session(v)) else { return };
            (Some(app.to_owned()), Some(std::str::from_utf8(session).expect("UUID is ASCII").to_owned()))
        };
        if (t.resume_app.as_ref(), t.resume_session.as_ref()) == (binding.0.as_ref(), binding.1.as_ref()) {
            return;
        }
        let was = (t.resume_app.clone(), t.resume_session.clone());
        (t.resume_app, t.resume_session) = binding;
        let bytes = serde_json::to_vec(&st.reg).expect("registry serializes");
        if let Err(e) = self.save_durable(bytes) {
            let t = st.reg.terminals.get_mut(&id).expect("live terminal");
            (t.resume_app, t.resume_session) = was;
            eprintln!("core: cannot save resume session for {}: {e}", crate::terminals::address(id));
        }
    }

    pub(crate) async fn resume_in_shell(&self, id: u32, scope: String, command: String) {
        let deadline = tokio::time::Instant::now() + crate::templates::ready_timeout();
        loop {
            let ready = {
                let st = self.lock();
                let Some(t) = st.reg.terminals.get(&id).filter(|t| t.is_live() && t.scope.as_ref() == Some(&scope))
                else {
                    return;
                };
                t.pid.is_some() && st.last_output.get(&id).is_some_and(|at| at.elapsed() >= crate::input::OUTPUT_SETTLE)
            };
            if ready {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                eprintln!("core: recovery shell for {} did not settle", crate::terminals::address(id));
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        let result = self
            .type_in(id, crate::input::quiet(), true, |st, t| {
                if t.scope.as_ref() != Some(&scope) || !t.keep || t.recovery.is_some() || t.status.is_some() {
                    return Err(RpcError::new("unavailable", "recovery shell changed before resume", ""));
                }
                let screen = &st.screens[&id];
                let mut steps = vec![crate::input::Step::Write(crate::input::paste(screen.dec_mode(2004), &command))];
                steps.extend(crate::input::submit_steps(screen, t)?);
                Ok(((), steps))
            })
            .await;
        if let Err(e) = result {
            eprintln!("core: cannot type recovery command for {}: {}", crate::terminals::address(id), e.message);
        }
    }

    pub(crate) fn set_keep(&self, id: u32, p: KeepParams) -> Result<Public, RpcError> {
        let mut st = self.lock();
        let t = st.reg.terminals.get(&id).ok_or_else(|| RpcError::invalid_params("terminal no longer exists"))?;
        if !t.is_live() {
            return Err(RpcError::invalid_params("only live terminals can be kept"));
        }
        if p.expected_scope.is_some() || p.recovery.is_some() {
            if p.expected_scope.as_deref().filter(|s| !s.is_empty()) != t.scope.as_deref() || t.scope.is_none() {
                return Err(RpcError::invalid_params("expected_scope must match the current program's HC_SCOPE"));
            }
            if st.stopping
                || self.restart.load(Ordering::SeqCst)
                || st.killing.contains(&id)
                || crate::platform::host_shutdown_in_progress()
            {
                return Err(RpcError::new("unavailable", "terminal is stopping", "retry in the new program"));
            }
            // The child can register before SPAWNED: its fresh scope and claimed holder identify this spawn.
            if t.holder.is_none() {
                return Err(RpcError::new("unavailable", "terminal program is lost", "retry in the new program"));
            }
        }
        if let Some(recovery) = &p.recovery {
            if !p.keep {
                return Err(RpcError::invalid_params("recovery requires keep=true"));
            }
            if recovery.argv.first().is_none_or(|a| a.is_empty())
                || recovery.argv.iter().any(|a| a.contains('\0'))
                || recovery.cwd.contains('\0')
                || !Path::new(&recovery.cwd).is_absolute()
                || !Path::new(&recovery.cwd).is_dir()
            {
                return Err(RpcError::invalid_params("recovery requires a direct argv and an existing absolute cwd"));
            }
        }
        let was = t.clone();
        let t = st.reg.terminals.get_mut(&id).expect("checked");
        t.keep = p.keep;
        if !p.keep {
            t.recovery = None;
            t.resume_session = None;
            t.resume_app = None;
        } else if let Some(recovery) = p.recovery {
            t.recovery = Some(recovery);
        }
        // A successful registration means it reached disk, before any acknowledgement or event.
        let bytes = serde_json::to_vec(&st.reg).expect("registry serializes");
        if let Err(e) = self.save_durable(bytes) {
            st.reg.terminals.insert(id, was);
            return Err(RpcError::internal(format!("cannot save keep configuration: {e}")));
        }
        let t = &st.reg.terminals[&id];
        let public = Public(t.clone());
        let lost = (!p.keep && t.holder.is_none()).then(|| t.reason.clone().unwrap_or_else(|| "holder_lost".into()));
        self.emit(
            &mut st,
            "keep",
            id,
            serde_json::json!({"keep": p.keep, "resume_session": public.0.resume_session.as_deref()}),
        );
        if let Some(reason) = lost {
            self.record_finish(&mut st, id, proto::Exit { code: None, signal: None }, &reason);
            self.changed.notify_waiters();
            self.save(&st);
        }
        Ok(Public(st.reg.terminals[&id].clone()))
    }
}

impl Terminal {
    pub(crate) fn osc_resume_command(&self, commands: &BTreeMap<String, String>) -> Option<String> {
        if self.recovery.is_some() {
            return None;
        }
        let (app, session) = (self.resume_app.as_ref()?, self.resume_session.as_ref()?);
        let command = commands.get(app).map(String::as_str).or_else(|| crate::platform::resume_command(app))?;
        (!command.trim().is_empty()).then(|| command.replace("{session}", session))
    }

    pub(crate) fn prepare_recovery(
        &mut self,
        env: &mut BTreeMap<String, String>,
        shell: &[String],
    ) -> (Vec<String>, String) {
        if let Some(recovery) = &self.recovery
            && Path::new(&recovery.cwd).is_dir()
            && self.reason.as_deref() != Some("recovery_spawn_failed")
        {
            return (recovery.argv.clone(), recovery.cwd.clone());
        }
        let cwd = self
            .last_cwd
            .as_ref()
            .filter(|p| Path::new(p).is_dir())
            .or_else(|| Path::new(&self.cwd).is_dir().then_some(&self.cwd))
            .cloned()
            .unwrap_or_else(|| {
                env.get("HOME").or_else(|| env.get("USERPROFILE")).cloned().unwrap_or_else(|| "/".into())
            });
        let argv = if let Some(recovery) = &self.recovery {
            let problem = if Path::new(&recovery.cwd).is_dir() { "could not start" } else { "folder is gone" };
            let notice = format!(
                "Recovery {problem}: {}. Resume: {}",
                recovery.cwd,
                crate::platform::recovery_command(&recovery.argv)
            );
            self.title = notice.clone();
            crate::platform::recovery_shell(shell, &notice, env)
        } else {
            shell.to_vec()
        };
        (argv, cwd)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::paths::Paths;
    use crate::terminals::NewTerminal;

    #[test]
    fn registration_is_scope_bound_durable_and_rolls_back_on_write_failure() {
        let root = std::env::temp_dir().join(format!("hc-recovery-{}", crate::platform::random_hex()));
        std::fs::create_dir_all(&root).unwrap();
        let core = Core::start(
            Paths {
                state: root.join("state"),
                config: root.join("config.toml"),
                socket: root.join("core.sock"),
                holder_socket: root.join("holder.sock"),
            },
            crate::api::CoreInfo { id: "test".into(), name: "test".into() },
        )
        .unwrap();
        let id = {
            let mut st = core.lock();
            let id = st.reg.create(NewTerminal { argv: vec!["unused".into()], ..Default::default() }, 0).unwrap();
            let t = st.reg.terminals.get_mut(&id).unwrap();
            t.scope = Some("current".into());
            t.holder = Some("holder".into());
            t.pid = Some(1);
            id
        };
        let recovery = Recovery { argv: vec!["helper".into(), "--recover".into()], cwd: root.display().to_string() };
        let params = || KeepParams {
            terminal: "t1".into(),
            keep: true,
            recovery: Some(recovery.clone()),
            expected_scope: Some("current".into()),
        };
        let mut stale = params();
        stale.expected_scope = Some("previous".into());
        assert_eq!(core.set_keep(id, stale).err().unwrap().kind, "invalid_params");
        let mut absent = params();
        absent.expected_scope = None;
        assert!(core.set_keep(id, absent).is_err());
        for (argv, cwd) in [
            (vec![], recovery.cwd.clone()),
            (vec!["".into()], recovery.cwd.clone()),
            (vec!["bad\0arg".into()], recovery.cwd.clone()),
            (vec!["helper".into()], "missing-relative".into()),
        ] {
            let mut invalid = params();
            invalid.recovery = Some(Recovery { argv, cwd });
            assert!(core.set_keep(id, invalid).is_err());
        }
        for state in ["lost", "finished", "stopping", "killing"] {
            {
                let mut st = core.lock();
                match state {
                    "lost" => st.reg.terminals.get_mut(&id).unwrap().holder = None,
                    "finished" => st.reg.terminals.get_mut(&id).unwrap().state = "finished".into(),
                    "stopping" => st.stopping = true,
                    _ => {
                        st.killing.insert(id);
                    }
                }
            }
            assert!(core.set_keep(id, params()).is_err(), "{state}");
            let mut st = core.lock();
            let t = st.reg.terminals.get_mut(&id).unwrap();
            t.pid = Some(1);
            t.holder = Some("holder".into());
            t.state = "live".into();
            st.stopping = false;
            st.killing.clear();
        }
        // A directory at the temp filename forces a real write failure on every OS.
        {
            let st = core.lock();
            core.save_durable(serde_json::to_vec(&st.reg).unwrap()).unwrap();
        }
        std::fs::create_dir(core.paths.state.join("terminals.tmp")).unwrap();
        assert_eq!(core.set_keep(id, params()).err().unwrap().kind, "internal");
        assert!(!core.lock().reg.terminals[&id].keep);
        assert!(core.lock().reg.terminals[&id].recovery.is_none());
        std::fs::remove_dir(core.paths.state.join("terminals.tmp")).unwrap();
        // A restarted child already has its fresh scope before the holder's SPAWNED acknowledgement.
        {
            let mut st = core.lock();
            let t = st.reg.terminals.get_mut(&id).unwrap();
            t.pid = None;
            t.reason = Some("holder_lost".into());
        }
        let public = core.set_keep(id, params()).unwrap();
        assert!(serde_json::to_value(public).unwrap().get("recovery").is_none());
        let disk: crate::terminals::Registry =
            crate::paths::read_state(&core.paths.state.join("terminals.json")).unwrap();
        assert_eq!(disk.terminals[&id].recovery.as_ref(), Some(&recovery));
        core.lock().stopping = true;
        let stopping_off =
            KeepParams { terminal: "t1".into(), keep: false, recovery: None, expected_scope: Some("current".into()) };
        assert_eq!(core.set_keep(id, stopping_off).err().unwrap().kind, "unavailable");
        assert!(core.lock().reg.terminals[&id].keep);
        assert_eq!(core.lock().reg.terminals[&id].recovery.as_ref(), Some(&recovery));
        core.lock().stopping = false;
        let stale_off =
            KeepParams { terminal: "t1".into(), keep: false, recovery: None, expected_scope: Some("previous".into()) };
        assert!(core.set_keep(id, stale_off).is_err());
        assert!(core.lock().reg.terminals[&id].keep);
        assert_eq!(core.lock().reg.terminals[&id].recovery.as_ref(), Some(&recovery));
        std::fs::create_dir(core.paths.state.join("terminals.tmp")).unwrap();
        assert!(
            core.set_keep(id, KeepParams { terminal: "t1".into(), keep: false, recovery: None, expected_scope: None })
                .is_err()
        );
        assert!(core.lock().reg.terminals[&id].keep);
        assert_eq!(core.lock().reg.terminals[&id].recovery.as_ref(), Some(&recovery));
        std::fs::remove_dir(core.paths.state.join("terminals.tmp")).unwrap();
        core.set_keep(
            id,
            KeepParams { terminal: "t1".into(), keep: false, recovery: None, expected_scope: Some("current".into()) },
        )
        .unwrap();
        let disk: crate::terminals::Registry =
            crate::paths::read_state(&core.paths.state.join("terminals.json")).unwrap();
        assert!(!disk.terminals[&id].keep);
        assert!(disk.terminals[&id].recovery.is_none());
        drop(core);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_selects_existing_folders_without_changing_registration() {
        let root = std::env::temp_dir().join(format!("hc-recovery-cwd-{}", crate::platform::random_hex()));
        let last = root.join("last");
        let launch = root.join("launch");
        std::fs::create_dir_all(&last).unwrap();
        std::fs::create_dir(&launch).unwrap();
        let mut reg = crate::terminals::Registry::default();
        let id = reg
            .create(
                NewTerminal { argv: vec!["unused".into()], cwd: launch.display().to_string(), ..Default::default() },
                0,
            )
            .unwrap();
        let t = reg.terminals.get_mut(&id).unwrap();
        t.last_cwd = Some(last.display().to_string());
        let recovery =
            Recovery { argv: vec!["helper".into(), "--recover".into()], cwd: root.join("gone").display().to_string() };
        t.recovery = Some(recovery.clone());
        let mut env = BTreeMap::from([("HOME".into(), root.display().to_string())]);
        let shell = vec!["configured-shell".into()];
        for expected in [&last, &launch, &root] {
            let (argv, cwd) = t.prepare_recovery(&mut env, &shell);
            assert_eq!(cwd, expected.display().to_string());
            assert_ne!(argv, recovery.argv);
            assert_eq!(t.recovery.as_ref(), Some(&recovery));
            if expected != &root {
                std::fs::remove_dir(expected).unwrap();
            }
        }
        std::fs::create_dir(&recovery.cwd).unwrap();
        assert_eq!(t.prepare_recovery(&mut env, &shell), (recovery.argv, recovery.cwd));
        std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn native_session_from_current_live_output_is_durable_independent_of_status() {
        let root = std::env::temp_dir().join(format!("hc-native-session-{}", crate::platform::random_hex()));
        let core = Core::start(
            Paths {
                state: root.join("state"),
                config: root.join("config.toml"),
                socket: root.join("core.sock"),
                holder_socket: root.join("holder.sock"),
            },
            crate::api::CoreInfo { id: "test".into(), name: "test".into() },
        )
        .unwrap();
        let id = {
            let mut st = core.lock();
            let id = st.reg.create(NewTerminal { argv: vec!["unused".into()], ..Default::default() }, 0).unwrap();
            let t = st.reg.terminals.get_mut(&id).unwrap();
            t.scope = Some("current".into());
            t.holder = Some("holder".into());
            t.pid = Some(1);
            t.keep = true;
            st.screens.insert(id, ghostty_vt::Terminal::new(80, 24, 1 << 20));
            core.output(&mut st, id, "holder", b"\x1b]7501;state=working:session=unregistered\x07");
            assert!(st.reg.terminals[&id].resume_session.is_none());
            id
        };
        let register = || KeepParams {
            terminal: "t1".into(),
            keep: true,
            recovery: Some(Recovery { argv: vec!["helper".into()], cwd: root.display().to_string() }),
            expected_scope: Some("current".into()),
        };
        let off = || KeepParams {
            terminal: "t1".into(),
            keep: false,
            recovery: None,
            expected_scope: Some("current".into()),
        };
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        core.subscribe(&mut core.lock(), crate::events::Filter::default(), crate::events::Sink::Waiter(tx));
        let disk_session = || {
            let reg: crate::terminals::Registry =
                crate::paths::read_state(&core.paths.state.join("terminals.json")).unwrap();
            reg.terminals[&id].resume_session.clone()
        };
        {
            let mut st = core.lock();
            core.output(&mut st, id, "stale-holder", b"\x1b]7501;state=working:session=stale\x07");
            assert!(st.reg.terminals[&id].resume_session.is_none());
            core.output(
                &mut st,
                id,
                "holder",
                b"\x1b]7501;state=working:app=codex:session=first: session = 01234567-89ab-cdef-0123-456789abc",
            );
            assert!(st.reg.terminals[&id].resume_session.is_none());
            core.output(&mut st, id, "holder", b"def  \x1b\\");
            assert_eq!(st.reg.terminals[&id].resume_session.as_deref(), Some("01234567-89ab-cdef-0123-456789abcdef"));
            let event = events.try_recv().unwrap();
            assert_eq!(event["type"], "status");
            assert_eq!(event["data"]["resume_session"], "01234567-89ab-cdef-0123-456789abcdef");
            assert_eq!(
                disk_session().as_deref(),
                Some("01234567-89ab-cdef-0123-456789abcdef"),
                "same status word still saves identity immediately"
            );
            for body in [
                "state=working:id=child:session=wrong",
                "state=working:id=child?:session=wrong",
                "state=working:app=codex:session=bad?",
                "state=working:app=codex:session=wrong:session=bad?",
            ] {
                core.output(&mut st, id, "holder", format!("\x1b]7501;{body}\x07").as_bytes());
                assert_eq!(
                    st.reg.terminals[&id].resume_session.as_deref(),
                    Some("01234567-89ab-cdef-0123-456789abcdef")
                );
            }
            for body in [
                b"\x1b]7501;state=working:id=child\xff:session=wrong\x07".as_slice(),
                b"\x1b]7501;state=working:app=codex:session=wrong\xff\x07".as_slice(),
            ] {
                core.output(&mut st, id, "holder", body);
                assert_eq!(
                    st.reg.terminals[&id].resume_session.as_deref(),
                    Some("01234567-89ab-cdef-0123-456789abcdef")
                );
            }
            core.output(
                &mut st,
                id,
                "holder",
                b"\x1b]7501;state=working:app=codex:unknown=\xff:session=01234567-89ab-cdef-0123-456789abcdef\x07",
            );
            assert_eq!(
                disk_session().as_deref(),
                Some("01234567-89ab-cdef-0123-456789abcdef"),
                "unknown binary fields do not suppress selected values"
            );
            core.output(
                &mut st,
                id,
                "holder",
                b"\x1b]7501;state=working:app=codex:session=01234567-89ab-cdef-0123-456789abcdef\x07",
            );
            std::fs::create_dir(core.paths.state.join("terminals.tmp")).unwrap();
            core.output(
                &mut st,
                id,
                "holder",
                b"\x1b]7501;state=working:app=codex:session=fedcba98-7654-3210-fedc-ba9876543210\x07",
            );
            let public = serde_json::to_value(Public(st.reg.terminals[&id].clone())).unwrap();
            assert_eq!(public["resume_session"], "01234567-89ab-cdef-0123-456789abcdef");
            assert_eq!(disk_session().as_deref(), Some("01234567-89ab-cdef-0123-456789abcdef"));
            std::fs::remove_dir(core.paths.state.join("terminals.tmp")).unwrap();
            core.output(
                &mut st,
                id,
                "holder",
                b"\x1b]7501;state=working:app=codex:session=fedcba98-7654-3210-fedc-ba9876543210\x07",
            );
            assert_eq!(disk_session().as_deref(), Some("fedcba98-7654-3210-fedc-ba9876543210"));
            // Native binding adds no immediate write for an unchanged ID; ordinary deferred status saving stays unchanged.
            let registry = core.paths.state.join("terminals.json");
            let saved = core.paths.state.join("saved.json");
            std::fs::rename(&registry, &saved).unwrap();
            core.output(
                &mut st,
                id,
                "holder",
                b"\x1b]7501;state=working:app=codex:session=fedcba98-7654-3210-fedc-ba9876543210\x07",
            );
            assert!(!registry.exists());
            std::fs::rename(&saved, &registry).unwrap();
        }
        {
            let mut st = core.lock();
            let shell = vec!["configured-shell".into()];
            let commands = BTreeMap::from([("custom".into(), "agent --resume={session}".into())]);
            for clear in
                ["state=clear", "state=idle:app=codex", "state=idle:session=01234567-89ab-cdef-0123-456789abcdef"]
            {
                core.output(
                    &mut st,
                    id,
                    "holder",
                    b"\x1b]7501;state=idle:app=custom:session=01234567-89ab-cdef-0123-456789abcdef\x07",
                );
                let t = st.reg.terminals.get_mut(&id).unwrap();
                assert_eq!(t.prepare_recovery(&mut BTreeMap::new(), &shell).0, shell);
                assert_eq!(
                    t.osc_resume_command(&commands).as_deref(),
                    Some("agent --resume=01234567-89ab-cdef-0123-456789abcdef")
                );
                assert!(t.osc_resume_command(&BTreeMap::new()).is_none());
                core.output(&mut st, id, "holder", format!("\x1b]7501;{clear}\x07").as_bytes());
                assert!(disk_session().is_none());
                let t = st.reg.terminals.get_mut(&id).unwrap();
                assert!(t.resume_app.is_none());
                assert_eq!(t.prepare_recovery(&mut BTreeMap::new(), &shell).0, shell);
            }
        }
        core.set_keep(id, off()).unwrap();
        core.set_keep(id, register()).unwrap();
        {
            let mut st = core.lock();
            assert!(st.reg.terminals[&id].resume_session.is_none());
            core.output(
                &mut st,
                id,
                "holder",
                b"\x1b]7501;state=working:app=codex:session=01234567-89ab-cdef-0123-456789abcdef\x07",
            );
            assert_eq!(disk_session().as_deref(), Some("01234567-89ab-cdef-0123-456789abcdef"));
            core.output(
                &mut st,
                id,
                "holder",
                b"\x1b]7501;state=working:app=codex:session=fedcba98-7654-3210-fedc-ba9876543210\x07",
            );
            assert_eq!(disk_session().as_deref(), Some("fedcba98-7654-3210-fedc-ba9876543210"));
        }
        // Recovery registration preserves the latest durable main identity.
        assert_eq!(
            core.set_keep(id, register()).unwrap().0.resume_session.as_deref(),
            Some("fedcba98-7654-3210-fedc-ba9876543210")
        );
        assert!(core.lock().reg.terminals[&id].osc_resume_command(&BTreeMap::new()).is_none());
        {
            let mut st = core.lock();
            for reason in ["holder_lost", "server_stop"] {
                st.reg.terminals.get_mut(&id).unwrap().holder = Some("holder".into());
                st.screens.insert(id, ghostty_vt::Terminal::new(80, 24, 1 << 20));
                core.output(
                    &mut st,
                    id,
                    "holder",
                    b"\x1b]7501;state=working:app=codex:session=fedcba98-7654-3210-fedc-ba9876543210\x07",
                );
                assert!(st.reg.terminals[&id].status.is_some());
                core.record_finish(&mut st, id, proto::Exit { code: None, signal: None }, reason);
                assert!(st.reg.terminals[&id].status.is_none());
                assert!(st.reg.terminals[&id].holder.is_none());
                assert_eq!(
                    st.reg.terminals[&id].resume_session.as_deref(),
                    Some("fedcba98-7654-3210-fedc-ba9876543210")
                );
                core.output(&mut st, id, "holder", b"\x1b]7501;state=working:session=late\x07");
                assert_eq!(
                    st.reg.terminals[&id].resume_session.as_deref(),
                    Some("fedcba98-7654-3210-fedc-ba9876543210")
                );
            }
            let t = st.reg.terminals.get_mut(&id).unwrap();
            t.prepare_recovery(&mut BTreeMap::new(), &["configured-shell".into()]);
            assert_eq!(t.resume_session.as_deref(), Some("fedcba98-7654-3210-fedc-ba9876543210"));
            t.holder = Some("holder".into());
            // Drain deferred status saves before occupying the writer's temporary path.
            core.save_durable(serde_json::to_vec(&st.reg).unwrap()).unwrap();
        }
        while events.try_recv().is_ok() {}
        std::fs::create_dir(core.paths.state.join("terminals.tmp")).unwrap();
        assert!(core.set_keep(id, off()).is_err());
        assert!(events.try_recv().is_err(), "failed persistence emits no binding change");
        assert_eq!(
            core.lock().reg.terminals[&id].resume_session.as_deref(),
            Some("fedcba98-7654-3210-fedc-ba9876543210")
        );
        std::fs::remove_dir(core.paths.state.join("terminals.tmp")).unwrap();
        assert!(core.set_keep(id, off()).unwrap().0.resume_session.is_none());
        let event = events.try_recv().unwrap();
        assert_eq!(event["type"], "keep");
        assert_eq!(event["data"]["resume_session"], serde_json::Value::Null);
        assert!(disk_session().is_none());
        drop(core);
        std::fs::remove_dir_all(root).unwrap();
    }
}

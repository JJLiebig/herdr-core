//! Private fork: explicit, vendor-neutral recovery on the existing kept lifecycle.
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
    /// Opaque invocation correlation token required for native session reports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_token: Option<String>,
}

fn osc_field<'a>(raw: &'a str, key: &str) -> Option<&'a str> {
    let whitespace = [' ', '\t', '\n', '\r', '\x0b', '\x0c'];
    raw.split(':')
        .filter_map(|field| {
            let (field_key, value) = field.split_once('=')?;
            (field_key.trim_matches(whitespace) == key).then(|| value.trim_matches(whitespace))
        })
        .next_back()
        .filter(|s| !s.is_empty())
}

fn osc_value<'a>(raw: &'a str, key: &str) -> Option<&'a str> {
    osc_field(raw, key).filter(|v| v.bytes().all(|c| c.is_ascii_alphanumeric() || b"_.+-,/=".contains(&c)))
}

impl Core {
    /// New live output only: replaying a tail could resurrect a deliberately reset session.
    pub(crate) fn bind_resume_session(&self, st: &mut State, id: u32, report: &ghostty_vt::StatusReport) {
        if !report.id.is_empty() || report.state == "clear" || osc_field(&report.raw, "id").is_some() {
            return;
        }
        let Some(session) = osc_value(&report.raw, "session") else { return };
        let Some(t) = st
            .reg
            .terminals
            .get_mut(&id)
            .filter(|t| t.is_live() && t.keep && t.recovery.is_some() && t.holder.is_some())
        else {
            return;
        };
        let Some(token) = t.recovery.as_ref().and_then(|r| r.session_token.as_deref()).filter(|t| !t.is_empty()) else {
            return;
        };
        if osc_value(&report.raw, "sessiontoken") != Some(token) {
            return;
        }
        if t.resume_session.as_deref() == Some(session) {
            return;
        }
        let was = t.resume_session.replace(session.into());
        let bytes = serde_json::to_vec(&st.reg).expect("registry serializes");
        if let Err(e) = crate::paths::write_atomic(&self.paths.state.join("terminals.json"), &bytes) {
            st.reg.terminals.get_mut(&id).expect("live terminal").resume_session = was;
            eprintln!("core: cannot save resume session for {}: {e}", crate::terminals::address(id));
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
        } else if let Some(recovery) = p.recovery {
            t.recovery = Some(recovery);
        }
        // A successful registration means it reached disk, before any acknowledgement or event.
        let bytes = serde_json::to_vec(&st.reg).expect("registry serializes");
        if let Err(e) = crate::paths::write_atomic(&self.paths.state.join("terminals.json"), &bytes) {
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
        Ok(public)
    }
}

impl Terminal {
    pub(crate) fn prepare_recovery(&mut self, env: &BTreeMap<String, String>) -> (Vec<String>, String) {
        if let Some(recovery) = &mut self.recovery {
            recovery.session_token = None;
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
        (crate::platform::own_shell(), cwd)
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
        let recovery = Recovery {
            argv: vec!["helper".into(), "--recover".into()],
            cwd: root.display().to_string(),
            session_token: None,
        };
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
            invalid.recovery = Some(Recovery { argv, cwd, session_token: None });
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
    fn configured_recovery_never_substitutes_a_missing_cwd() {
        let mut reg = crate::terminals::Registry::default();
        let id = reg.create(NewTerminal { argv: vec!["unused".into()], ..Default::default() }, 0).unwrap();
        let t = reg.terminals.get_mut(&id).unwrap();
        t.recovery = Some(Recovery {
            argv: vec!["helper".into(), "--recover".into()],
            cwd: "gone-folder".into(),
            session_token: None,
        });
        assert_eq!(
            t.prepare_recovery(&BTreeMap::new()),
            (vec!["helper".into(), "--recover".into()], "gone-folder".into())
        );
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
            core.output(
                &mut st,
                id,
                "holder",
                b"\x1b]7501;state=working:session=unregistered:sessiontoken=current-token\x07",
            );
            assert!(st.reg.terminals[&id].resume_session.is_none());
            id
        };
        let register = || KeepParams {
            terminal: "t1".into(),
            keep: true,
            recovery: Some(Recovery {
                argv: vec!["helper".into()],
                cwd: root.display().to_string(),
                session_token: Some("current-token".into()),
            }),
            expected_scope: Some("current".into()),
        };
        let off = || KeepParams {
            terminal: "t1".into(),
            keep: false,
            recovery: None,
            expected_scope: Some("current".into()),
        };
        let mut ordinary = register();
        ordinary.recovery.as_mut().unwrap().session_token = None;
        core.set_keep(id, ordinary).unwrap();
        {
            let mut st = core.lock();
            core.output(
                &mut st,
                id,
                "holder",
                b"\x1b]7501;state=working:session=unregistered:sessiontoken=current-token\x07",
            );
            assert!(st.reg.terminals[&id].resume_session.is_none(), "ordinary recipes do not bind native sessions");
        }
        core.set_keep(id, register()).unwrap();
        let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
        core.subscribe(&mut core.lock(), crate::events::Filter::default(), crate::events::Sink::Waiter(tx));
        let disk_session = || {
            let reg: crate::terminals::Registry =
                crate::paths::read_state(&core.paths.state.join("terminals.json")).unwrap();
            reg.terminals[&id].resume_session.clone()
        };
        {
            let mut st = core.lock();
            core.output(
                &mut st,
                id,
                "stale-holder",
                b"\x1b]7501;state=working:session=stale:sessiontoken=current-token\x07",
            );
            assert!(st.reg.terminals[&id].resume_session.is_none());
            core.output(&mut st, id, "holder", b"\x1b]7501;state=working:sessiontoken=wrong: sessiontoken = current-token :session=first: session = pri");
            assert!(st.reg.terminals[&id].resume_session.is_none());
            core.output(&mut st, id, "holder", b"mary  \x1b\\");
            assert_eq!(st.reg.terminals[&id].resume_session.as_deref(), Some("primary"));
            let event = events.try_recv().unwrap();
            assert_eq!(event["type"], "status");
            assert_eq!(event["data"]["resume_session"], "primary");
            assert_eq!(disk_session().as_deref(), Some("primary"), "same status word still saves identity immediately");
            for body in [
                "state=working:id=child:session=wrong:sessiontoken=current-token",
                "state=working:id=child?:session=wrong:sessiontoken=current-token",
                "state=working:session=bad?:sessiontoken=current-token",
                "state=working:session=wrong:session=bad?:sessiontoken=current-token",
                "state=clear:session=wrong:sessiontoken=current-token",
                "state=working:session=:sessiontoken=current-token",
                "state=working:sessiontoken=current-token",
                "state=working:session=wrong: session = :sessiontoken=current-token",
                "state=working:session=wrong",
                "state=working:session=wrong:sessiontoken=wrong",
                "state=working:session=wrong:sessiontoken=current-token: sessiontoken = ",
            ] {
                core.output(&mut st, id, "holder", format!("\x1b]7501;{body}\x07").as_bytes());
                assert_eq!(st.reg.terminals[&id].resume_session.as_deref(), Some("primary"));
            }
            std::fs::create_dir(core.paths.state.join("terminals.tmp")).unwrap();
            core.output(
                &mut st,
                id,
                "holder",
                b"\x1b]7501;state=working:session=changed:sessiontoken=current-token\x07",
            );
            let public = serde_json::to_value(Public(st.reg.terminals[&id].clone())).unwrap();
            assert_eq!(public["resume_session"], "primary");
            assert_eq!(disk_session().as_deref(), Some("primary"));
            std::fs::remove_dir(core.paths.state.join("terminals.tmp")).unwrap();
            core.output(
                &mut st,
                id,
                "holder",
                b"\x1b]7501;state=working:session=changed:sessiontoken=current-token\x07",
            );
            assert_eq!(disk_session().as_deref(), Some("changed"));
            // Native binding adds no immediate write for an unchanged ID; ordinary deferred status saving stays unchanged.
            let registry = core.paths.state.join("terminals.json");
            let saved = core.paths.state.join("saved.json");
            std::fs::rename(&registry, &saved).unwrap();
            core.output(
                &mut st,
                id,
                "holder",
                b"\x1b]7501;state=working:session=changed:sessiontoken=current-token\x07",
            );
            assert!(!registry.exists());
            std::fs::rename(&saved, &registry).unwrap();
            // Start an old report before the explicit reset, and complete it after the next invocation registers.
            core.output(&mut st, id, "holder", b"\x1b]7501;state=working:sessiontoken=current-token:session=old-");
        }
        core.set_keep(id, off()).unwrap();
        let mut next = register();
        next.recovery.as_mut().unwrap().session_token = Some("next-token".into());
        core.set_keep(id, next).unwrap();
        {
            let mut st = core.lock();
            core.output(&mut st, id, "holder", b"partial\x07");
            assert!(st.reg.terminals[&id].resume_session.is_none(), "old partial report cannot cross reset");
            core.output(
                &mut st,
                id,
                "holder",
                b"\x1b]7501;state=working:session=old-complete:sessiontoken=current-token\x07",
            );
            assert!(st.reg.terminals[&id].resume_session.is_none(), "old queued complete report cannot cross reset");
            core.output(&mut st, id, "holder", b"\x1b]7501;state=working:session=changed:sessiontoken=next-token\x07");
            assert_eq!(disk_session().as_deref(), Some("changed"));
        }
        // Recovery rotates the token without erasing the last durable native identity.
        assert_eq!(core.set_keep(id, register()).unwrap().0.resume_session.as_deref(), Some("changed"));
        {
            let mut st = core.lock();
            core.output(
                &mut st,
                id,
                "holder",
                b"\x1b]7501;state=working:session=stale-recovery:sessiontoken=next-token\x07",
            );
            assert_eq!(st.reg.terminals[&id].resume_session.as_deref(), Some("changed"));
            for reason in ["holder_lost", "server_stop"] {
                st.reg.terminals.get_mut(&id).unwrap().holder = Some("holder".into());
                st.screens.insert(id, ghostty_vt::Terminal::new(80, 24, 1 << 20));
                core.output(&mut st, id, "holder", b"\x1b]7501;state=working\x07");
                assert!(st.reg.terminals[&id].status.is_some());
                core.record_finish(&mut st, id, proto::Exit { code: None, signal: None }, reason);
                assert!(st.reg.terminals[&id].status.is_none());
                assert!(st.reg.terminals[&id].holder.is_none());
                assert_eq!(st.reg.terminals[&id].resume_session.as_deref(), Some("changed"));
                core.output(
                    &mut st,
                    id,
                    "holder",
                    b"\x1b]7501;state=working:session=late:sessiontoken=current-token\x07",
                );
                assert_eq!(st.reg.terminals[&id].resume_session.as_deref(), Some("changed"));
            }
            // A replacement claim must invalidate the old token before its helper can re-register.
            let t = st.reg.terminals.get_mut(&id).unwrap();
            t.prepare_recovery(&BTreeMap::new());
            assert!(t.recovery.as_ref().unwrap().session_token.is_none());
            assert_eq!(t.resume_session.as_deref(), Some("changed"));
            t.holder = Some("holder".into());
            st.screens.insert(id, ghostty_vt::Terminal::new(80, 24, 1 << 20));
            core.output(
                &mut st,
                id,
                "holder",
                b"\x1b]7501;state=working:session=late-before-registration:sessiontoken=current-token\x07",
            );
            assert_eq!(st.reg.terminals[&id].resume_session.as_deref(), Some("changed"));
        }
        while events.try_recv().is_ok() {}
        std::fs::create_dir(core.paths.state.join("terminals.tmp")).unwrap();
        assert!(core.set_keep(id, off()).is_err());
        assert!(events.try_recv().is_err(), "failed persistence emits no binding change");
        assert_eq!(core.lock().reg.terminals[&id].resume_session.as_deref(), Some("changed"));
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

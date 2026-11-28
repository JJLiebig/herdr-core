//! Lost kept terminals stay live without a holder until recovery starts.
use crate::holder_proto as proto;
use crate::runtime::{Core, now_ms};
use crate::terminals::{KEEP_RETRY_MS, Terminal};
use std::io::{self, Write};
use std::sync::Arc;
use std::sync::atomic::Ordering;

impl Terminal {
    pub(crate) fn schedule_restart(&mut self, now: u64) {
        let stable = self.program_started_ms.is_some_and(|at| now.saturating_sub(at) >= 10_000);
        self.restart_delay_ms = if stable {
            KEEP_RETRY_MS
        } else {
            match self.restart_delay_ms {
                0 => KEEP_RETRY_MS,
                KEEP_RETRY_MS => 3000,
                3000 => 6000,
                6000 => 12_000,
                12_000 => 30_000,
                _ => 60_000,
            }
        };
        self.restart_after_ms = now + self.restart_delay_ms;
        self.program_started_ms = None;
    }
}

/// Under what the lost program left on screen, so it isn't taken for something still running.
const RESTORED: &[u8] = b"\x1b[0m\r\n\x1b[2m-- the program above ended; this is a new shell --\x1b[0m\r\n";
const RESUMING: &[u8] = b"\x1b[0m\r\n\x1b[2m-- the program above ended; starting it again --\x1b[0m\r\n";

impl Core {
    pub(crate) fn spawn_failed(&self, st: &mut crate::runtime::State, id: u32) {
        let t = st.reg.terminals.get_mut(&id).expect("failed spawn");
        if let Some(resume) = t.resume.take() {
            crate::resume::remove_launch(&self.paths.state, resume.launch.as_deref());
        }
        t.holder = None;
        t.pid = None;
        if st.killing.contains(&id) || !t.keep || t.reason.as_deref().is_none_or(|r| r == "spawn_pending") {
            let reason = t.reason.clone().filter(|r| r != "spawn_pending").unwrap_or_else(|| "holder_lost".into());
            self.record_finish(st, id, proto::Exit { code: None, signal: None }, &reason);
        } else {
            if t.recovery.as_ref().is_some_and(|r| r.argv == t.argv) {
                t.reason = Some("recovery_spawn_failed".into());
            }
            t.schedule_restart(now_ms());
        }
        self.save(st);
        self.changed.notify_waiters();
    }

    pub(crate) async fn recover_kept(self: Arc<Self>, startup: bool) {
        let pending = self.lock().reg.terminals.values().any(|t| t.is_live() && t.keep && t.holder.is_none());
        if !pending || crate::platform::host_shutdown_in_progress() {
            return;
        }
        if self.sweeps.load(Ordering::SeqCst) != 0 || self.lock().stopping || self.restart.load(Ordering::SeqCst) {
            return;
        }
        if let Err(e) = self.ensure_holder().await {
            let _ = writeln!(io::stderr(), "core: kept terminals waiting for a holder: {e}");
            return;
        }
        let (shell, commands, restore_screen) = match crate::config::load(&self.paths.config).and_then(|settings| {
            Ok((settings.shell()?, settings.resume_commands, settings.restore_screen.unwrap_or(false)))
        }) {
            Ok(value) => value,
            Err(e) => {
                let _ = writeln!(io::stderr(), "core: kept terminals waiting for a shell: {}", e.message);
                return;
            }
        };
        let base_env = crate::platform::program_env().await;
        let mut st = self.lock();
        if st.stopping
            || self.restart.load(Ordering::SeqCst)
            || self.sweeps.load(Ordering::SeqCst) != 0
            || crate::platform::host_shutdown_in_progress()
        {
            return;
        }
        let ids: Vec<u32> = st
            .reg
            .terminals
            .values()
            .filter(|t| {
                t.is_live()
                    && t.keep
                    && t.holder.is_none()
                    && (startup || t.restart_after_ms <= now_ms())
                    && !self.normal_sweep_pending(t)
            })
            .map(|t| t.id())
            .collect();
        for id in ids {
            let Some(holder) = st.current() else { break };
            let instance = holder.instance.clone();
            let t = st.reg.terminals.get_mut(&id).expect("pending");
            let previous = (t.recovery.is_some() || t.resume_app.is_some()).then(|| t.clone());
            t.title.clear();
            let mut env = crate::api::child_env(base_env.clone(), t.restart_env.clone());
            t.identity = crate::api::terminal_identity(&env);
            let shell_command = t.osc_resume_command(&commands);
            let (fallback, cwd) = t.prepare_recovery(&mut env, &shell);
            let mut resume = crate::resume::Resume::new(&self.paths.state);
            let attempt = t
                .resume
                .take()
                .filter(|_| resume.is_some())
                .and_then(|old| old.prepare(&self.paths.state, id, &shell, &cwd));
            let (launch, argv, cwd) = match attempt {
                Some((file, argv, cwd)) => (Some(file), argv, cwd),
                None => (None, fallback, cwd),
            };
            let resuming = launch.is_some();
            let (env, scope) = crate::api::terminal_env(&self, id, &cwd, env, resume.as_ref());
            if let Some(resume) = &mut resume {
                resume.rearmed = resuming;
                resume.launch = launch;
            }
            t.resume = resume;
            t.native_recovery = false;
            t.scope = Some(scope.clone());
            t.holder = Some(instance);
            t.argv = if resuming { shell.clone() } else { argv.clone() };
            t.cwd = cwd.clone();
            t.template = None;
            t.reported = false;
            t.full_screen = false;
            t.win32_input = false;
            t.printed = 0;
            t.screen = "exact".into();
            t.submit = None;
            t.stop = None;
            t.paste_gap_ms = None;
            let spawn = proto::Spawn { argv, env, cwd, cols: t.cols, rows: t.rows };
            let identity = t.identity.clone();
            if let Some(previous) = previous {
                let bytes = serde_json::to_vec(&st.reg).expect("registry serializes");
                if let Err(e) = self.save_durable(bytes) {
                    st.reg.terminals.insert(id, previous);
                    let _ = writeln!(
                        io::stderr(),
                        "core: cannot save recovery claim for {}: {e}",
                        crate::terminals::address(id)
                    );
                    continue;
                }
            } else {
                self.save(&st);
            }
            if !restore_screen {
                let _ = std::fs::remove_file(self.snapshot_path(id));
            }
            // A retry after a failed spawn keeps the screen the first try built.
            let mut screen = st.screens.remove(&id).filter(|_| restore_screen).unwrap_or_else(|| {
                let mut screen = ghostty_vt::Terminal::new(spawn.cols, spawn.rows, self.history_bytes());
                if restore_screen && let Some(old) = self.take_saved_screen(id) {
                    screen.write(&old.replay());
                    screen.write(if resuming { RESUMING } else { RESTORED });
                    // ConPTY paints a new console from the top left: move what was there into history.
                    if cfg!(windows) {
                        screen.write(&b"\n".repeat(spawn.rows as usize));
                        screen.write(b"\x1b[H");
                    }
                }
                screen
            });
            screen.set_xtversion(&identity);
            st.screens.insert(id, screen);
            st.last_output.remove(&id);
            if let Some(link) = st.link(id) {
                link.send(&proto::json_frame(proto::SPAWN, id, &spawn));
                if let Some(command) = shell_command {
                    let core = self.clone();
                    tokio::spawn(async move {
                        core.resume_in_shell(id, scope, command).await;
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::terminals::{NewTerminal, Registry};

    #[test]
    fn retries_back_off_forever_and_reset_only_after_a_stable_program() {
        let mut reg = Registry::default();
        let id = reg.create(NewTerminal { argv: vec!["sh".into()], keep: true, ..Default::default() }, 0).unwrap();
        let t = reg.terminals.get_mut(&id).unwrap();
        for delay in [1500, 3000, 6000, 12_000, 30_000, 60_000, 60_000] {
            t.schedule_restart(100_000);
            assert_eq!(t.restart_delay_ms, delay);
            assert_eq!(t.restart_after_ms, 100_000 + delay);
        }
        t.program_started_ms = Some(90_001);
        t.schedule_restart(100_000);
        assert_eq!(t.restart_delay_ms, 60_000, "a short-lived program does not reset retries");
        t.program_started_ms = Some(90_000);
        t.schedule_restart(100_000);
        assert_eq!(t.restart_delay_ms, 1500);
        t.schedule_restart(100_000);
        assert_eq!(t.restart_delay_ms, 3000);
        assert!(t.is_live());
    }
}

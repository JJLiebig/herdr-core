use crate::runtime::Core;
use crate::terminals::Terminal;

impl Core {
    /// The holder reported the program's exit. Capture its scope before the
    /// record can be removed or a kept terminal can start another program.
    pub(super) fn sweep_ended_terminal(&self, terminal: &Terminal) {
        if crate::platform::needs_terminal_scope_sweep()
            && terminal.is_live()
            && let Some(scope) = &terminal.scope
        {
            let scope = scope.clone();
            let pending = self.normal_sweeps.clone();
            pending.lock().unwrap().insert(scope.clone());
            std::thread::spawn(move || {
                crate::sweep::sweep(std::slice::from_ref(&scope));
                pending.lock().unwrap().remove(&scope);
            });
        }
    }

    pub(crate) fn normal_sweep_pending(&self, terminal: &Terminal) -> bool {
        terminal.scope.as_ref().is_some_and(|scope| self.normal_sweeps.lock().unwrap().contains(scope))
    }

    pub(super) fn normal_sweeps_pending(&self) -> bool {
        !self.normal_sweeps.lock().unwrap().is_empty()
    }
}

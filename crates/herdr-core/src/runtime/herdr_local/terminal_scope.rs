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
            self.sweep_in_background(vec![scope.clone()]);
        }
    }
}

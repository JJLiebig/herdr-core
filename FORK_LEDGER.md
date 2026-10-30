# Personal fork ledger

Current differences from `herdrdev/herdr-core`; one row per maintained
capability, not per commit. Merge upstream verbatim, then check these seams.

| Capability | Owned files | Required upstream seams | Retirement condition |
| --- | --- | --- | --- |
| Fork maintenance | Top section of `AGENTS.md`, this file | None | Fork no longer maintained |
| Explicit kept recovery | `crates/herdr-core/src/herdr_local/recovery.rs` (command, durable registration and native session binding, selection, tests) | Module declarations; terminal recovery/resume_session fields/init/filter; `KeepParams` fields; `terminal_keep` delegation; `recover_kept` preparation (explicit argv/cwd, configured-shell fallback); current-holder output/status binding calls and existing status/keep event/mirror fields; Rust Ghostty raw callback field/copy; `.gitmodules` and pinned Ghostty fork; manual/schema; canonical Windows PTY lifecycle test | Upstream supports a durable scope-bound direct recovery command and native session binding with the same lifecycle, or personal helpers no longer need recovery |
| Planned restart environment (upstream PR #21) | `crates/herdr-core/src/restart.rs`, `crates/herdr-core/src/platform/environment_tests.rs`, `crates/herdr-core/tests/releases_server.py` | API/server/client/module delegation; platform environment/private-file helpers and systemd handoff; manual and lifecycle, update, input and readiness tests | Retire when upstream PR #21 merges (or upstream ships equivalent) |
| Ordered template submission keys (upstream PR #31) | `crates/herdr-core/src/input.rs` (`Core::send`), `crates/herdr-core/tests/m7.rs`, ordered submission test in `crates/herdr-core/tests/windows.rs`, `docs/templates.md` | Terminal submission delegation; manual and generated schema descriptions | Retire when upstream PR #31 merges (or upstream ships equivalent) |
| Configurable default shell (upstream PR #32) | `crates/herdr-core/src/config.rs` shell setting/resolution; shell tests in `crates/herdr-core/tests/{settings,keep,windows}.rs`; `README.md`, `docs/commands.md` | New-terminal default argv in `api/run.rs`; holder special command handling; kept recovery loads the setting and passes it to the fork recovery selector; manual/help | Retire when upstream PR #32 merges (or upstream ships equivalent) |
| Templates use the machine's shell line (upstream PR #35) | Template-shell test in `crates/herdr-core/tests/windows.rs`, `crates/herdr-core/tests/m7.rs`; `docs/templates.md` | Template argv in `api/run.rs` uses `platform::shell_line`; manual | Retire when upstream PR #35 merges (or upstream ships equivalent) |
| Clean Windows child environment (upstream PR #39) | `crates/herdr-core/src/platform/windows_env.rs` (validation), environment tests in `crates/herdr-core/tests/windows.rs` | Env validation and Windows Terminal identity removal in `api/run.rs`, `platform/{mod,unix,windows}.rs`; manual | Retire when upstream PR #39 merges (or upstream ships equivalent) |
| Real Windows shell folder and command line (upstream PR #40) | `crates/herdr-core/src/platform/windows_process.rs`, process tests in `crates/herdr-core/tests/windows.rs` | `foreground.rs` reads; `Pty::foreground` in `platform/windows.rs`; module declaration in `platform/mod.rs`; manual | Retire when upstream PR #40 merges (or upstream ships equivalent) |

Recovery uses the existing same-ID keep lifecycle; ordinary keep still starts a shell.
Agent integrations and session selection remain outside core.

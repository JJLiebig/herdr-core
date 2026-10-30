# Personal fork ledger

Current differences from `herdrdev/herdr-core`; one row per maintained
capability, not per commit. Merge upstream verbatim, then check these seams.

| Capability | Owned files | Required upstream seams | Retirement condition |
| --- | --- | --- | --- |
| Fork maintenance | Top section of `AGENTS.md`, this file | None | Fork no longer maintained |
| Explicit kept recovery | `crates/herdr-core/src/herdr_local/recovery.rs` (command, durable registration and native session binding, selection, tests) | Module declarations; terminal recovery/resume_session fields/init/filter; `KeepParams` fields; `terminal_keep` delegation; `recover_kept` preparation (argv/cwd); current-holder output/status binding calls and existing status/keep event/mirror fields; Rust Ghostty raw callback field/copy; `.gitmodules` and pinned Ghostty fork; manual/schema; canonical Windows PTY lifecycle test | Upstream supports a durable scope-bound direct recovery command and native session binding with the same lifecycle, or personal helpers no longer need recovery |
| Planned restart environment (upstream PR #21) | `crates/herdr-core/src/restart.rs`, `crates/herdr-core/src/platform/environment_tests.rs`, `crates/herdr-core/tests/releases_server.py` | API/server/client/module delegation; platform environment/private-file helpers and systemd handoff; manual and lifecycle, update, input and readiness tests | Retire when upstream PR #21 merges (or upstream ships equivalent) |
| Ordered template submission keys (upstream PR #31) | `crates/herdr-core/src/input.rs` (`Core::send`), `crates/herdr-core/tests/m7.rs`, ordered submission test in `crates/herdr-core/tests/windows.rs`, `docs/templates.md` | Terminal submission delegation; manual and generated schema descriptions | Retire when upstream PR #31 merges (or upstream ships equivalent) |

Recovery uses the existing same-ID keep lifecycle; ordinary keep still starts a shell.
Agent integrations and session selection remain outside core.

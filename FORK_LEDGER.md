# Personal fork ledger

Current differences from `herdrdev/herdr-core`; one row per maintained
capability, not per commit. Merge upstream verbatim, then check these seams.

| Capability | Owned files | Required upstream seams | Retirement condition |
| --- | --- | --- | --- |
| Fork maintenance | Top section of `AGENTS.md`, this file | None | Fork no longer maintained |
| Explicit kept recovery | `crates/herdr-core/src/herdr_local/recovery.rs` (command, durable registration and native session binding, selection, tests) | Module declarations; terminal recovery/resume_session fields/init/filter; `KeepParams` fields; `terminal_keep` delegation; `recover_kept` preparation (argv/cwd); current-holder output/status binding calls and existing status/keep event/mirror fields; Rust Ghostty raw callback field/copy; `.gitmodules` and pinned Ghostty fork; manual/schema; canonical Windows PTY lifecycle test | Upstream supports a durable scope-bound direct recovery command and native session binding with the same lifecycle, or personal helpers no longer need recovery |
| Planned restart environment | `crates/herdr-core/src/restart.rs`, `src/platform/environment_tests.rs`, and `tests/releases_server.py` beneath the same crate | API/server/client/module delegation; platform environment/private-file helpers; manual and existing lifecycle, update, input and readiness tests | Retire the fork copy when herdrdev/herdr-core#21 lands. Keep the reviewed upstream proposal's paths verbatim while pending to avoid move-only divergence |

Recovery uses the existing same-ID keep lifecycle; ordinary keep still starts a shell.
Agent integrations and session selection remain outside core.

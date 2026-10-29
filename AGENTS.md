# Personal fork rules

This is `JJLiebig/herdr-core`, based on `herdrdev/herdr-core`. Within this
repository's guidance, these rules override conflicting instructions below.
The upstream instructions follow unchanged.

- Keep the maintained difference from upstream small. Core stays a generic
  runtime: agent names, session hooks and resume selection belong in personal
  helpers, never in core. Status remains OSC 7501.
- Keep fork logic in focused capability files under the owning module's
  `herdr_local/` directory. Upstream files are integration seams: change only
  the fields, declarations, schema entries and calls the capability needs.
  Do not add abstractions or widen visibility just to isolate a few lines.
- Fix only requested fork behavior, regressions it introduces and required
  integration seams. Report unrelated upstream defects; review findings do not
  expand scope. Ask before absorbing a larger upstream subsystem.
- Merge `upstream/master` directly into this fork's `master` with a real merge
  commit; no upstream-sync PR. Check affected fork seams and resolve conflicts
  narrowly; do not rewrite upstream behavior or include cleanup.
- Review only fork-authored changes or exact conflict resolutions, never the
  full upstream integration. Use local review; do not request GitHub AI reviews.
  Run relevant checks and required native gates; serialize heavy Rust builds.
- Keep `FORK_LEDGER.md` as a current capability map, not a changelog. Update a
  capability's owned files, required seams and retirement condition together.
- User-approved private fork work does not wait for upstream approval. Changes
  proposed to Can still follow his contribution and shared-API rules. Do not
  open upstream issues or PRs unless requested.
- Do not add fork branding, release pipelines, dependency churn or compatibility
  layers without a concrete need. Reuse existing holders and recovery rules.
- `origin` is the public fork. `private-archive` preserves the old private copy.
  Never mirror-push or publish old private branches wholesale: deleted files
  remain in history. Publish only audited changes based on public upstream.
- Keep machine configuration, credentials, captured conversations and test
  artifacts out of commits. Exercise recovery only in isolated core sessions.

---

# herdr-core

If AGENTS.local.md exists, read it too: the maintainer's local rules.

`herdr-core` (alias `hc`) is a terminal runtime for coding agents: it runs
programs in terminals, keeps them alive, relays raw bytes to any viewer,
reads status from OSC 7501, and lets agents start, talk to, read, wait on and
get notified by other agents. It draws nothing.

Read the code. This file only holds what reading won't tell you: the rules,
the traps, and the handful of decisions that look arbitrary until you know
why.

Can (`ogulcancelik`) maintains it. The code is authoritative; ask the
maintainer about conflicting requirements.

`hc --skill` prints the manual in `crates/herdr-core/src/SKILL.md`. The
installable skill is the short stub in `skills/herdr-core/SKILL.md`: it says
when to use hc and points to `hc --skill`, so it rarely changes.
Keep the manual current when changing agent-facing behavior.

## The rule

Core is a runtime, not a multiplexer. If a feature only makes sense with a
layout or a workspace to return to, it does not belong here.

## Rules that aren't in the code

- Fresh code. Do not copy from Herdr 1
  ([herdrdev/herdr](https://github.com/herdrdev/herdr)). Read it for ideas,
  lessons and edge cases.
- libghostty-vt comes from fresh upstream Ghostty master, not Herdr's
  vendored copy.
- Status comes only from OSC 7501. No screen detection, no agent registry,
  no vendor-specific logic. This is the one most likely to be helpfully
  added; don't.
- Every action reports what was observed, never what was assumed.
- One API; the CLI is one call per command. The schema is generated from one
  method table, so params and results in `api/types.rs` are also CLI args and
  the published schema. Change them and regenerate:
  `HC_UPDATE_SCHEMA=1 cargo test -p herdr-core --test m0`.
- Lean, KISS, always. Fewer lines, fewer files, fewer dependencies. No
  abstraction before a second real caller, no config for things nobody asked
  to configure.
- Deciding code stays separate from IO, so it tests without processes.
- Gate per OS only where the OS really behaves differently, and keep it in
  `platform/` (`unix.rs`, `windows.rs`) behind one interface: shared code
  calls `platform::…` and has no `#[cfg]` of its own. A platform quirk gets
  a test on its platform, and other platforms keep their behavior.
- TDD. A test first for every required documented behavior. One good test per
  behavior, not five checking the same thing. A few end-to-end tests with real
  PTYs beat many mocks. The suite runs in seconds; keep it that way.
- Do not change documented behavior silently. Behavior changes need maintainer
  agreement.
- Aim for source files under ~32 KB, hard cap 48 KB because read tools stop
  near 50. Split along a real seam when you next touch one that's over.

## Traps

- Tests and smokes run in isolated sessions (`HC_SESSION`) with scratch XDG
  paths under `/var/tmp`. Never touch the machine's real core, Herdr, or its
  running agents. Clean up scratch data.
- `just install` replaces the installed binary and restarts core. Running
  terminals are kept.
- Real-agent smokes (Claude Code with haiku, Pi with a cheap model) cost
  money. Ask before running them.
- A dependency change (crates or Ghostty) needs `just notices`; a test fails
  if a crate is missing from the notices.
- `rustfmt.toml` width 120 is the format and CI checks it: `just fmt`.
  `just lint` is fmt, clippy, and clippy for Windows from Linux. `just check`
  is lint plus the whole suite. Run it before proposing a commit.

## Contributing

Work on a branch and open a pull request to `master`. CI must pass and Can
approves; merges are squashed. Commit messages: lowercase conventional
commits, single line, no AI co-author lines. Only Can releases (a `v*` tag).

# Personal fork ledger

Current differences from `herdrdev/herdr-core`; one row per maintained
capability, not per commit. Merge upstream verbatim, then check these seams.

| Capability | Owned files | Required upstream seams | Retirement condition |
| --- | --- | --- | --- |
| Fork maintenance | Top section of `AGENTS.md`, this file | None | Fork no longer maintained |

Generic recovery commands are developed in `crates/herdr-core/src/herdr_local/`.
Record their tested behavior and seams here when delivered. Agent integrations
remain outside core.

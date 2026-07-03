# ikigai-org

The org-mode agenda as ikigai ROC resources. `urn:org:agenda:{period}` (today ·
tomorrow · week · month · month names · `YYYY-MM` · `YYYY-MM-DD`; bare = week)
reads **date-fixed events** — headlines with active `<…>` timestamps, repeaters
(`+1w`, `+1y`, …) expanded into the window — from org files, and serves them as
text or as the **same skolemized Turtle event graph** `urn:personal:calendar`
speaks (`urn:event:{uid}`, `ical:` vocabulary, `ik:calendar` provenance, no
blank nodes) — so org and native calendars **union and diff as graphs**.

The org files are read **through the kernel**: the host binds them (e.g. an
`ikigai-fs` space jailed to the org directory at `urn:orgfile:{path}`) and hands
this space their IRIs — capability-gated, wasm-clean, golden-thread-ready.

Event identity: an org `:ID:` property wins; otherwise a stable FNV-1a of
`title|timestamp`, with repeater occurrences date-suffixed (`…-2026-07-03`).

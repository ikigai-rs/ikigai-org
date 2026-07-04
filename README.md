# ikigai-org

The org-mode agenda as ikigai ROC resources. `urn:org:agenda:{period}` (today ·
tomorrow · week · month · year · month names · `YYYY-MM` · `YYYY-MM-DD` · an
end-inclusive range `YYYY-MM-DD..YYYY-MM-DD`; bare = week) reads **date-fixed
events** — headlines with active `<…>` timestamps, repeaters
(`+1w`, `+1y`, …) expanded into the window, and `<start>--<end>` ranges as
ONE spanning event (all-day across days, or a continuous timed block; a
range straddling the window edge is kept) — from org files, and serves them as
text or as the **same skolemized Turtle event graph** `urn:personal:calendar`
speaks (`urn:event:{uid}`, `ical:` vocabulary, `ik:calendar` provenance, no
blank nodes) — so org and native calendars **union and diff as graphs**.

The org files are read **through the kernel**: the host binds them (e.g. an
`ikigai-fs` space jailed to the org directory at `urn:orgfile:{path}`) and hands
this space their IRIs — capability-gated, wasm-clean, golden-thread-ready.

Event identity: an org `:ID:` property wins; otherwise a stable FNV-1a of
`title|timestamp`, with repeater occurrences date-suffixed (`…-2026-07-03`).

Alarms: an `:ALERT:` line in a headline's section (`:ALERT: 1h 1d` — space or
comma separated, `m`/`h`/`d` suffixes, bare numbers are minutes) or an org
`:APPT_WARNTIME:` property becomes multi-valued `ik:alert` (minutes before
start) on the event — the same property `urn:personal:calendar` reads and
writes, so alarms survive the graph round-trip onto a derived calendar.

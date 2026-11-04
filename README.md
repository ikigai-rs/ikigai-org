# ikigai-org

The org-mode agenda and [TODO entries](#todo-entries) as ikigai ROC resources. `urn:org:agenda:{period}` (today ·
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
Because its doors serve whatever files it was handed, `space(files)` claims no
name (no `urn:iki:space:org`): the host names the instance it built.

Endpoint ids (for arrangement `door` declarations):

| Pattern | Stable id |
| --- | --- |
| `urn:org:agenda` (default week) | `org-agenda` |
| `urn:org:agenda:{period}` | `org-agenda-period` |

In 0.3.0, the explicit-period door changes from `org-agenda` to
`org-agenda-period`; update arrangements selecting that door. Both ids follow
core's IRI-safety requirements and kebab-case convention, and each selects exactly one door.
The host supplies `urn:orgfile:{path}`; this crate does not bind a file endpoint.
Give that host mount the id `orgfile` (rather than `file`, which belongs to
ikigai-fs's `urn:file:{path}` door) when composing both mounts in one kernel.

Caching follows the files and the clock. An absolute period (`YYYY-MM`,
`YYYY-MM-DD`, a range) is a function of the org files alone, so it is cached
under their golden threads whenever the host's file space is cacheable (an edit
that cuts a file's thread recomputes it; over a live file space it is live too).
A relative period (`today`, `week`, a month name) also depends on the date,
which comes from the **kernel's clock** — so it is cached only until the next
local midnight, and not at all on a clockless kernel.

Event identity: an org `:ID:` property wins; otherwise a stable FNV-1a of
`title|timestamp`, with repeater occurrences date-suffixed (`…-2026-07-03`).

Todo states: an open `TODO` headline stays on the calendar, keyword and all
(the reminder is deliberate); `DONE` keeps the event under its clean name —
the calendar records that it happens, org records that it's complete;
`CANCELLED` events leave the calendar on the next derive.

Alarms: an `:ALERT:` line in a headline's section (`:ALERT: 1h 1d` — space or
comma separated, `m`/`h`/`d` suffixes, bare numbers are minutes) or an org
`:APPT_WARNTIME:` property becomes multi-valued `ik:alert` (minutes before
start) on the event — the same property `urn:personal:calendar` reads and
writes, so alarms survive the graph round-trip onto a derived calendar.

Conformance: `tests/conformance.rs` runs
[`ikigai-conformance`](https://github.com/ikigai-rs/ikigai-conformance) over a
fixture kernel (an in-memory org file space at `urn:orgfile:{path}`) and passes
with no opt-outs — typed inputs, the declared `text/turtle` face skolemized
under `urn:event:{uid}` with only `ical:` and `ik:` terms the vocabulary
defines, and the cache contract above pinned by hand: edit + cut recomputes, no
cut serves stale, a relative period expires at midnight, a live file space
leaves the agenda live. The space is declared host-named, and `SPACE-NAME`
holds it to claiming nothing. `REQUIRED` (0.8.0) runs and has nothing to
drop: every input is a binding, defaulted, or optional, and a test pins that.
`BODIES` (0.12.1) compares `invoke` with `invoke_body` over the kernel's own
root space; `BODY-INPUTS` has nothing to probe, since no argument reads a body.
`tests/todo.rs` walks the TODO doors over the real `ikigai-fs` endpoint, with no
opt-outs, and pins the round trip, the conflicts and the capabilities by hand.

## TODO entries

`ikigai_org::todo_space(files)` serves the headlines with a TODO keyword, from files
the host binds by name (`TodoFile::new("queue", "urn:orgfile:queue.org")`). The file
is the only state: a state changed in Emacs is the next read's answer.

| Pattern | Stable id | Verb | Requires | Answers |
| --- | --- | --- | --- | --- |
| `urn:org:todo` | `org-todo` | Source | `urn:cap:fs:read:*` | every entry |
| `urn:org:todo:id:{id}` | `org-todo-id` | Source | `urn:cap:fs:read:*` | one entry, or `NotFound` |
| `urn:org:todo:state:{state}` | `org-todo-state` | Source | `urn:cap:fs:read:*` | the entries in that state |
| `urn:org:todo:tag:{tag}` | `org-todo-tag` | Source | `urn:cap:fs:read:*` | the entries with that tag |
| `urn:org:todo:file:{name}` | `org-todo-file` | Sink | `urn:cap:fs:read:*`, `urn:cap:fs:write:*` | appends one entry |

The Sink needs read as well as write because it reads the file, as the caller, to
find the version it writes against. The file space's own path rules still judge the
grant.

**Reading.** The keywords are the file's own `#+TODO:` line (`TODO NEXT | DONE`,
fast-access marks ignored), defaulting to `TODO | DONE`. As in Emacs, a headline
starts at column zero, and an entry's metadata is the planning lines and drawers
directly under it, so a `:ID:` in the body is text. An entry with no `:ID:` gets a
stable FNV-1a id of its file and title, which survives a state change but not a
retitling. Tags are the headline's own: no inheritance, no `#+FILETAGS:`. A child
headline is its own entry, not part of its parent's body. Every read is cacheable
under the files' threads, and live over a live file space.

The list is one line per entry, and an id read is one labeled line per field with the
body last and verbatim:

```text
org todo — all entries

  NEXT  Read the draft  :ID: draft-1  tags: review, hub

org todo — draft-1

state: NEXT
title: Read the draft
tags: review, hub
file: queue
closed: (none)
logbook: (none)
body:
See [[https://example.com/draft][the draft]].
```

`as=text/turtle` gives `ical:Vtodo` nodes at `urn:org:todo:id:{id}`, no blank nodes:
`ical:uid`, `ical:summary`, `ical:status`, `ical:categories`, `ical:completed`,
`ical:description` and `ik:calendar`. The vocabulary has two gaps: nothing holds an
org keyword, so `ical:status` carries `NEXT` where iCalendar expects its own closed
list (nothing says which keywords are done), and nothing holds a logbook note, so
notes are on the text face only.

**Appending.** A Sink at `urn:org:todo:file:{name}` takes `id` and `title`, and
optionally `state` (default: the file's first open keyword), `tags`, `under` (a
top-level heading to file it beneath as a child) and `content` (the body). The new
text is the old bytes with one entry spliced in, nothing else, at the end of the file
or of that subtree. It refuses:

- `Conflict`: the file changed since the read, the `id` already exists in any bound
  file, or `under` names no heading or several. Nothing was written. Read again, and
  decide again.
- `InvalidArgument`: a `state` not among the file's keywords, a title ending in what
  org reads as tags, or a body that would change how the file reads (a headline at
  column zero, a live `#+TODO:` line).
- `Unsupported`: the file space does not declare `if-match`. Nothing was written.

**This needs `ikigai-fs` 0.4.1 or later.** Earlier versions
ignore `if-match`, which would make the write a blind overwrite, so the Sink asks
`urn:kernel:validate` first and refuses them.

**What it does not promise.** The comparison and the rename are one operation only to
writers through `ikigai-fs`. Emacs saving or Dropbox syncing takes no lock, so a save
landing in the instant between the comparison and the rename is lost. Any other edit
since the read is a `Conflict`, and the entry is what is refused, never the edit. The
window cannot be closed from this crate. Unsaved text in an Emacs buffer is Emacs's to
keep: it warns on saving over a file changed on disk.

On a cacheable mount, a read cached across an edit gives a stale version, so the Sink
answers `Conflict` until the host's watcher cuts the file's thread. That is safe, but
such a mount needs the watcher for appends to go through at all.

## Accessibility

The agenda and TODO text this crate writes, the rules it follows, how it is tested and its known limitations are in [ACCESSIBILITY.md](ACCESSIBILITY.md). Report a barrier with an issue carrying the `accessibility` label.

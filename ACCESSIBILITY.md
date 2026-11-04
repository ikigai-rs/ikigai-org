# Accessibility

What `ikigai-org` emits that people read, the rules that output follows, how it is tested, and
how to report a barrier. It is not a conformance claim. Adapted from the
[ACCESSIBILITY.md template](https://github.com/mgifford/ACCESSIBILITY.md) (MIT).

| Field | Value |
|---|---|
| Target | WCAG 2.2 Level AA, for any page that presents this output; this repository writes text only (plain text and Turtle), no HTML or SVG |
| Status | Evaluation in progress. No conformance claim. |
| Owner | the maintainers of this repository |
| Report a barrier | [a GitHub issue](https://github.com/ikigai-rs/ikigai-org/issues/new?labels=accessibility) with the `accessibility` label; anything security-sensitive through [private vulnerability reporting](https://github.com/ikigai-rs/ikigai-org/security/advisories/new) |
| Last reviewed | 2026-11-04 |

## What it emits that people read

- **The agenda as text** (`format_detail` in `src/lib.rs`), the default face of
  `urn:org:agenda[:{period}]`: an `org agenda — <period>` heading (with `· matching "<q>"` when
  searched), then one line per event: date, time or `all-day`, title, `[<org file>]` and
  `@ <location>`; `(no events)` when there are none.
- **Refusals** (`Error::Endpoint` in `src/lib.rs`): an unknown period, no org files configured,
  a file IRI that does not parse, a file that is not UTF-8.
- **The Turtle event graph** (`as=text/turtle`), which people read through other tools.
- **The TODO list** (`format_list` in `src/todo.rs`), the default face of `urn:org:todo` and its
  state and tag partitions: an `org todo — <what>` heading, then one line per entry: state, title,
  `:ID: <id>` and `tags: <tags>`; `(no entries)` when there are none.
- **One TODO entry** (`format_entry` in `src/todo.rs`), the default face of
  `urn:org:todo:id:{id}`: an `org todo — <id>` heading, then labeled lines `state:`, `title:`,
  `tags:`, `file:`, `closed:`, `logbook:` and `body:`, the body last and as written.
- **The Sink's answer** (`appended urn:org:todo:id:<id> to <name>`) **and TODO refusals** (in
  `src/todo.rs`): an id that already exists; a `state`, `title`, `tags`, `id` or body that cannot
  be written, each naming that input; no such bound file or `under` heading; two entries claiming
  one id; a file space with no conditional write.
- **The Turtle entry graph** (`as=text/turtle`), which people read through other tools.

Not this repository's: the titles, locations and links, which are the org files' text as
written (an open `TODO` keyword stays in the title on purpose; `DONE` is dropped); a TODO entry's
title, tags, logbook notes and body, which are the same; refusals from the host's file space,
which this crate passes through, including the `Conflict` naming a content id when the file
changed during an append; and any page or terminal a host puts this text in.

## The rules that output follows

- Nothing is said by color or layout alone: an all-day event says `all-day`, an empty period
  says `(no events)`, and a stay of several days is one line with its inclusive date range
  (`2026-07-14..2026-07-17  all-day`), so it does not read as its first day. No color is written.
- A refusal names the agenda resource and the input at fault. An unknown period lists every
  period form that works; no org files configured says which settings to add.
- A TODO entry's empty field says `(none)`, never a blank: `tags: (none)`, `closed: (none)`. Each
  field of one entry has a word for a label rather than a position. Only the body runs on, and it
  comes last, so everything after `body:` is the body.
- A TODO refusal that left the file alone says `nothing was written`, and a `state` not in the
  file lists the file's keywords.

These are followed by reading the source; the tests below pin some of them.

## How it is tested

- `src/lib.rs` unit tests: `a_date_range_is_one_spanning_event` pins the inclusive range line,
  and `location_and_url_drawers_are_parsed_and_reset_per_headline` pins a detail line with its
  `[file]` and `@ location`.
- `tests/conformance.rs` reads the text face and checks event titles are present; it does not
  check the format.
- `src/todo.rs` unit test `the_text_faces_say_everything_in_words` and `tests/todo.rs`'s
  `the_text_faces_and_partitions` pin the list and entry text, `(no entries)` and `(none)`
  exactly; `required_inputs_are_refused`, `an_id_already_present_is_a_conflict_wherever_it_is` and
  `a_file_space_with_no_conditional_write_is_refused_before_anything_is_written` pin the refusals'
  words.
- **No automated accessibility checker, browser or assistive technology has been used on this
  output.**

## Known limitations

Found by reading the source on 2026-10-31:

- A timed event that ends on a later day (an overnight block, a class running past midnight) is
  shown with its start date and `HH:MM-HH:MM` only, so `22:00-06:00` reads as one day.
- Times are local wall-clock time, 24-hour, with no time zone stated; dates are `YYYY-MM-DD` with
  no weekday.
- Fields are told apart by spacing and punctuation (`[…]` for the file, `@` for the location),
  and a one-day `all-day` is padded with spaces to line up a column. A screen reader may read the
  symbols aloud or skip them, and the alignment assumes a fixed-width font.
- The `(no events)` and agenda refusal texts are not pinned by any test.
- A TODO list line tells its fields apart by two spaces and the `:ID:` and `tags:` labels; the
  state and the title have no label there, only their order. The `:ID:` is read aloud with its
  colons, and it keeps org's spelling because that is what a reader searches the file for.
- A `closed:` time is org's text as written, with no time zone. An entry's body is org markup as
  written, so a link reads as `[[target][text]]`.
- A TODO state is the file's own keyword. Whether `WAIT` is open or finished is not said on any
  face; only the `#+TODO:` line says so.
- All text is English, with no way to choose another language.

## Contributing

Read this file before changing any output listed above: the agenda or TODO text or the refusals. Keep
meaning in words, never in color or position alone. Say in the pull request what was tested,
with what, and what was not.

## Reporting a barrier

[Open an issue](https://github.com/ikigai-rs/ikigai-org/issues/new?labels=accessibility) with
the `accessibility` label (the link sets it when GitHub lets you add labels; otherwise a
maintainer adds it): what you were doing, what happened, and your terminal or other tool and any
assistive technology, if you are comfortable sharing them. Anything security-sensitive goes
through [private vulnerability reporting](https://github.com/ikigai-rs/ikigai-org/security/advisories/new).
**We acknowledge a report within 7 days**, and promise nothing further.

## Change history

| Date | Change |
|---|---|
| 2026-10-31 | First version. |
| 2026-11-04 | Added the TODO faces, the Sink's answer and its refusals. |

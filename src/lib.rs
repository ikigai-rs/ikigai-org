//! `ikigai-org` — the org-mode agenda as ROC resources.
//!
//! `urn:org:agenda:{period}` reads **date-fixed events** (headlines with active
//! `<…>` timestamps, repeaters expanded into the window) from org files and
//! serves them as text or as the **same skolemized Turtle event graph**
//! `urn:personal:calendar` speaks — so org and native calendars union and diff
//! as graphs (the Brian-Busy materialized-view plan).
//!
//! The org files are read **through the kernel**: the host binds them (e.g. an
//! `ikigai-fs` space jailed to the org directory at `urn:orgfile:{path}`) and
//! hands this space their IRIs. That keeps this crate free of filesystem
//! access — capability-gated, wasm-clean, and golden-thread-ready when the
//! host's file space is cacheable.
//!
//! ## What is parsed (v1)
//! Headlines (`* Title`) whose section carries an active timestamp:
//! `<YYYY-MM-DD [Day] [HH:MM[-HH:MM]] [+N{d,w,m,y}]>`. Inactive `[…]`
//! timestamps are ignored. A `<start>--<end>` pair is one spanning event.
//! Untimed stamps are all-day; a timed stamp without
//! an end defaults to one hour. Repeaters (`+1w`, `+1y`, …) are expanded into
//! the requested window. Drawer properties: `:ID:` (identity), `:ALERT:` /
//! `:APPT_WARNTIME:` (alarms), `:LOCATION:` (place), `:URL:` (the join link a
//! derived calendar copy carries) and `:ZOOM_PASSCODE:` (which rides the same
//! `ical:description` as the link).
//!
//! ## An entry is read WHOLE, then emitted
//! A headline's properties belong to its event wherever they sit in the entry —
//! `SCHEDULED:` above the drawer reads exactly like a drawer above the stamp.
//! Emission is therefore two passes over the entry's lines (properties, then
//! stamps), never one line-by-line pass: see [`agenda_events`].

use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use ikigai_core::{
    ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri, ReprType,
    Representation, Result, UriTemplate, Verb,
};
use std::collections::HashSet;

/// One agenda event, normalized. The same shape the calendar side speaks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OrgEvent {
    /// Stable id: fnv1a of `title|raw-timestamp` (plus the occurrence date for
    /// repeater expansions) — deterministic across runs while the entry is
    /// unedited. An explicit org `:ID:` property, when present, wins.
    pub uid: String,
    /// The headline text.
    pub title: String,
    /// The source file's short name (provenance), e.g. `calendar.org`.
    pub source: String,
    /// Start/end as RFC 3339 local timestamps.
    pub start: String,
    pub end: String,
    /// Date-only timestamp.
    pub all_day: bool,
    /// The location (`:LOCATION:` drawer property).
    pub location: Option<String>,
    /// The join link (`:URL:` drawer property) — a Teams/Zoom URL the derived
    /// calendar copy should carry.
    pub url: Option<String>,
    /// The meeting passcode (`:ZOOM_PASSCODE:`), carried beside the link in
    /// [`OrgEvent::description`] so the phone has both at meeting time.
    pub passcode: Option<String>,
    /// Alarms: minutes before start (`:ALERT: 1h 1d` / `:APPT_WARNTIME: 30`).
    pub alerts: Vec<u32>,
}

impl OrgEvent {
    /// The `ical:description` this event carries: the join link and the meeting
    /// passcode, on **one line**.
    ///
    /// Single-line is a hard requirement, not a style choice. This string
    /// round-trips org → desired graph → EKEvent `.notes` → read back → source
    /// graph, and the deriver compares the two graphs BYTE-WISE (its
    /// `normalize_for_diff` canonicalizes `ical:location` whitespace, but
    /// deliberately keeps `ical:description` exact). The two Turtle serializers
    /// disagree about newlines — this crate flattens `\n` to a space,
    /// `ikigai-personal` escapes it as `\n` — so a multi-line description would
    /// render differently on the two sides and every linked event would
    /// delete-recreate forever (cli #151/#152/#154, the failure `DeriveBreaker`
    /// exists for). A string containing no `\r`/`\n` is invariant under BOTH
    /// serializers, which makes the round trip an identity.
    ///
    /// The link stays the FIRST whitespace-delimited token: the ingest side
    /// recovers a `:URL:` by scanning notes for a meeting link.
    pub fn description(&self) -> Option<String> {
        match (&self.url, &self.passcode) {
            // Link-only renders EXACTLY as 0.1.5 rendered it — adding this field
            // must not churn the events that already carry a link.
            (Some(url), None) => Some(url.clone()),
            (Some(url), Some(code)) => Some(format!("{url} | Passcode: {code}")),
            (None, Some(code)) => Some(format!("Passcode: {code}")),
            (None, None) => None,
        }
    }
}

/// Parse an `:ALERT:` value — space/comma-separated friendly durations
/// (`30m`, `1h`, `1d`, or bare minutes), sorted and deduplicated.
fn parse_alerts(value: &str) -> Vec<u32> {
    let mut alerts: Vec<u32> = value
        .split([' ', ','])
        .filter(|part| !part.is_empty())
        .filter_map(|part| {
            let (digits, factor) = match part.strip_suffix(['m', 'h', 'd']) {
                Some(rest) => (
                    rest,
                    match part.chars().last() {
                        Some('h') => 60,
                        Some('d') => 1440,
                        _ => 1,
                    },
                ),
                None => (part, 1),
            };
            digits.parse::<u32>().ok().map(|n| n * factor)
        })
        .collect();
    alerts.sort_unstable();
    alerts.dedup();
    alerts
}

// ---- period math (mirrors urn:personal:calendar's grammar) --------------------

fn period_range(period: &str, today: NaiveDate) -> Result<(NaiveDate, NaiveDate, String)> {
    let day = |d: NaiveDate| (d, d + Duration::days(1), format!("{d}"));
    let months = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    let range = match period {
        "today" => day(today),
        "tomorrow" => day(today + Duration::days(1)),
        "week" => {
            let monday = today - Duration::days(today.weekday().num_days_from_monday() as i64);
            (
                monday,
                monday + Duration::days(7),
                format!("week of {monday}"),
            )
        }
        "month" => month_range(today.year(), today.month()),
        "year" => {
            let jan1 = NaiveDate::from_ymd_opt(today.year(), 1, 1).expect("jan 1");
            let next = NaiveDate::from_ymd_opt(today.year() + 1, 1, 1).expect("jan 1");
            (jan1, next, format!("{}", today.year()))
        }
        name if months.contains(&name) => {
            let month = months.iter().position(|m| *m == name).expect("matched") as u32 + 1;
            month_range(today.year(), month)
        }
        other => {
            // A range: <start>..<end>, end-date INCLUSIVE (humans say "through").
            if let Some((from, to)) = other.split_once("..") {
                match (from.parse::<NaiveDate>(), to.parse::<NaiveDate>()) {
                    (Ok(from), Ok(to)) if to >= from => {
                        (from, to + Duration::days(1), format!("{from}..{to}"))
                    }
                    _ => return Err(bad_period(other)),
                }
            } else if let Ok(date) = other.parse::<NaiveDate>() {
                day(date)
            } else if let Some((y, m)) = other
                .split_once('-')
                .and_then(|(y, m)| Some((y.parse::<i32>().ok()?, m.parse::<u32>().ok()?)))
            {
                if !(1..=12).contains(&m) {
                    return Err(bad_period(other));
                }
                month_range(y, m)
            } else {
                return Err(bad_period(other));
            }
        }
    };
    Ok(range)
}

fn bad_period(period: &str) -> Error {
    Error::Endpoint(format!(
        "urn:org:agenda:{period}: unknown period — try today, tomorrow, week, month, year, \
         a month name, YYYY-MM, YYYY-MM-DD, or YYYY-MM-DD..YYYY-MM-DD"
    ))
}

fn month_range(year: i32, month: u32) -> (NaiveDate, NaiveDate, String) {
    let start = NaiveDate::from_ymd_opt(year, month, 1).expect("valid month");
    let end = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .expect("valid month");
    (start, end, format!("{year}-{month:02}"))
}

// ---- the org parser ------------------------------------------------------------

/// One parsed active timestamp.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Stamp {
    date: NaiveDate,
    time: Option<(NaiveTime, Option<NaiveTime>)>,
    /// Repeater as (count, unit) — `+2w` = (2, 'w').
    repeat: Option<(u32, char)>,
    /// Range end from `<start>--<end>` (org's multi-day event syntax).
    end_date: Option<NaiveDate>,
    end_time: Option<NaiveTime>,
    raw: String,
}

/// Parse the inside of one `<…>` active timestamp.
fn parse_stamp(inner: &str) -> Option<Stamp> {
    let mut date = None;
    let mut time = None;
    let mut repeat = None;
    for part in inner.split_whitespace() {
        if date.is_none() {
            if let Ok(d) = part.parse::<NaiveDate>() {
                date = Some(d);
                continue;
            }
            return None; // the first token must be the date
        }
        if let Some(rest) = part.strip_prefix('+') {
            // A repeater like +1w / +2d / +1y (org's ++/.+ cadences are treated
            // the same for expansion purposes).
            let rest = rest.trim_start_matches('+').trim_start_matches('.');
            if let (Some(unit), Ok(n)) = (
                rest.chars().last().filter(|c| "dwmy".contains(*c)),
                rest[..rest.len().saturating_sub(1)].parse::<u32>(),
            ) {
                repeat = Some((n.max(1), unit));
            }
            continue;
        }
        if part.contains(':') {
            // HH:MM or HH:MM-HH:MM
            let (from, to) = match part.split_once('-') {
                Some((a, b)) => (a, Some(b)),
                None => (part, None),
            };
            let parse_t = |t: &str| NaiveTime::parse_from_str(t, "%H:%M").ok();
            if let Some(start) = parse_t(from) {
                time = Some((start, to.and_then(parse_t)));
            }
            continue;
        }
        // anything else (the day name) is decorative
    }
    date.map(|date| Stamp {
        date,
        time,
        repeat,
        end_date: None,
        end_time: None,
        raw: inner.trim().to_string(),
    })
}

/// Every `<…>` active timestamp in a line (inactive `[…]` ignored). A
/// `<start>--<end>` pair — org's multi-day event syntax — merges into ONE
/// stamp carrying the range end, not two separate events.
fn stamps_in(line: &str) -> Vec<Stamp> {
    let mut found: Vec<Stamp> = Vec::new();
    let mut rest = line;
    let mut pending_range = false; // the previous stamp was followed by `--`
    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        if let Some(stamp) = parse_stamp(&rest[open + 1..open + close]) {
            match (pending_range, found.last_mut()) {
                (true, Some(prev)) if prev.end_date.is_none() => {
                    prev.end_date = Some(stamp.date);
                    prev.end_time = stamp.time.map(|(from, _)| from);
                    prev.raw = format!("{}--{}", prev.raw, stamp.raw);
                }
                _ => found.push(stamp),
            }
        }
        rest = &rest[open + close + 1..];
        pending_range = rest.starts_with("--");
    }
    found
}

/// FNV-1a — a tiny, stable hash for deterministic event ids (std's hasher is
/// not stable across releases).
fn fnv1a(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in text.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

/// Step a date by one repeater interval.
fn step(date: NaiveDate, repeat: (u32, char)) -> NaiveDate {
    let (n, unit) = repeat;
    match unit {
        'd' => date + Duration::days(n as i64),
        'w' => date + Duration::weeks(n as i64),
        'm' => add_months(date, n),
        'y' => add_months(date, n * 12),
        _ => date + Duration::days(n as i64),
    }
}

fn add_months(date: NaiveDate, months: u32) -> NaiveDate {
    let zero_based = date.month0() + months;
    let year = date.year() + (zero_based / 12) as i32;
    let month = zero_based % 12 + 1;
    let day = date.day();
    // clamp into the target month (Jan 31 + 1m -> Feb 28/29)
    (1..=day)
        .rev()
        .find_map(|d| NaiveDate::from_ymd_opt(year, month, d))
        .expect("day 1 always valid")
}

fn rfc3339(date: NaiveDate, time: NaiveTime) -> String {
    Local
        .from_local_datetime(&NaiveDateTime::new(date, time))
        .earliest()
        .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_else(|| format!("{date}T{time}"))
}

/// What one line means when it starts a headline.
enum Head<'a> {
    /// Not a headline — an ordinary line of the current entry.
    Not,
    /// A headline whose event is suppressed (`CANCELLED`). It still ENDS the
    /// previous entry: its lines belong to it, not to the entry above.
    Skip,
    /// A headline that carries events, with the title the calendar should show.
    Title(&'a str),
}

/// Classify a line as a headline (`* Title`, any number of stars then a space).
///
/// Todo states: an open TODO stays on the calendar keyword and all (the reminder
/// is wanted). DONE keeps the event under its clean name — the calendar records
/// that it happens; org records that it's complete. CANCELLED isn't happening:
/// no event, and the derive removes any existing one.
fn headline(line: &str) -> Head<'_> {
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix('*') else {
        return Head::Not;
    };
    let Some(title) = rest.trim_start_matches('*').strip_prefix(' ') else {
        return Head::Not;
    };
    let title = title.trim();
    if let Some(done) = title.strip_prefix("DONE ") {
        return Head::Title(done.trim());
    }
    if ["CANCELLED", "CANCELED", "DONE"]
        .iter()
        .any(|kw| title == *kw || title.starts_with(&format!("{kw} ")))
    {
        return Head::Skip;
    }
    Head::Title(title)
}

/// The properties of one org entry, gathered from its WHOLE body before any
/// event is emitted (see [`collect_props`]).
#[derive(Default)]
struct EntryProps {
    org_id: Option<String>,
    location: Option<String>,
    url: Option<String>,
    passcode: Option<String>,
    alerts: Vec<u32>,
}

/// A property line, as `(KEY, value)` — org's `:KEY: value` syntax, with the
/// value trimmed. Recognizing the general shape (not just the keys we consume)
/// keeps an unread property like `:ATTENDEES:` from being mistaken for body
/// text, which would close the preamble early.
fn property(trimmed: &str) -> Option<(&str, &str)> {
    let (key, value) = trimmed.strip_prefix(':')?.split_once(':')?;
    let named = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    (!key.is_empty() && key.chars().all(named)).then(|| (key, value.trim()))
}

/// A drawer value as a guaranteed single-line, non-empty string. Drawer values
/// come from `str::lines()` so they cannot already contain `\n`/`\r`; the
/// replacement is insurance for the round-trip invariant that
/// [`OrgEvent::description`] documents, not a live code path.
fn single_line(value: &str) -> Option<String> {
    let value = value.replace(['\r', '\n'], " ");
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// Gather an entry's properties from its whole body, in two admissible places:
///
/// - **inside a `:PROPERTIES:`…`:END:` drawer**, wherever that drawer sits. This
///   is what fixes the ordering bug: Emacs writes the drawer *below* a
///   `SCHEDULED:` line, and those properties are still the entry's.
/// - **in the entry's preamble** — before any body text — for drawer-less
///   properties, which is where org's own `:ALERT:`/`:APPT_WARNTIME:` habitually
///   sit (right after `:END:`, above the stamp).
///
/// Properties in the BODY are ignored on purpose. An entry's body can be
/// untrusted invite text (the ingest side pastes a captured event's notes there,
/// which is why it puts the body last), and a `:ID:` line in that text must not
/// be able to seize the entry's identity. Reading the whole entry for properties
/// would hand it exactly that, so "whole entry" means drawer plus preamble, not
/// every line.
fn collect_props(lines: &[&str]) -> EntryProps {
    let mut props = EntryProps::default();
    let mut in_drawer = false;
    let mut preamble = true;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.eq_ignore_ascii_case(":PROPERTIES:") {
            in_drawer = true;
            continue;
        }
        if trimmed.eq_ignore_ascii_case(":END:") {
            in_drawer = false;
            continue;
        }
        if trimmed.is_empty() {
            continue;
        }
        if let Some((key, value)) = property(trimmed) {
            if in_drawer || preamble {
                match key {
                    "ID" => props.org_id = single_line(value),
                    "LOCATION" => props.location = single_line(value),
                    "URL" => props.url = single_line(value),
                    "ZOOM_PASSCODE" => props.passcode = single_line(value),
                    "ALERT" => props.alerts = parse_alerts(value),
                    // org's own appointment-warning property: bare minutes.
                    "APPT_WARNTIME" => {
                        if let Ok(minutes) = value.parse::<u32>() {
                            props.alerts = vec![minutes];
                        }
                    }
                    _ => {}
                }
            }
            continue;
        }
        // Anything else is content: the stamp line, or body text. Either way the
        // preamble is over and only a real drawer still speaks for the entry.
        preamble = false;
    }
    props
}

/// Every active timestamp in an entry, in document order. Drawer contents are
/// metadata, not agenda lines, so a stamp-looking drawer value is never an
/// event; comments and drawer-less property lines are skipped for the same
/// reason.
fn entry_stamps(lines: &[&str]) -> Vec<Stamp> {
    let mut stamps = Vec::new();
    let mut in_drawer = false;
    for line in lines {
        let trimmed = line.trim();
        if trimmed.eq_ignore_ascii_case(":PROPERTIES:") {
            in_drawer = true;
            continue;
        }
        if trimmed.eq_ignore_ascii_case(":END:") {
            in_drawer = false;
            continue;
        }
        if in_drawer || trimmed.starts_with("# ") || trimmed.starts_with("#+") {
            continue;
        }
        if property(trimmed).is_some() {
            continue;
        }
        stamps.extend(stamps_in(line));
    }
    stamps
}

/// Make `uid` unique within one entry.
///
/// Two stamps under a single headline that carries an explicit `:ID:` would
/// otherwise emit the SAME `urn:event:` subject twice — one merged,
/// self-contradicting event in the desired graph, which the deriver can never
/// converge on. The first occurrence keeps the bare id (so a single-stamp entry,
/// which is every real one today, is untouched); later ones take the occurrence
/// date, then a counter.
fn unique_uid(uid: String, date: NaiveDate, seen: &mut HashSet<String>) -> String {
    if seen.insert(uid.clone()) {
        return uid;
    }
    let dated = format!("{uid}-{date}");
    if seen.insert(dated.clone()) {
        return dated;
    }
    let mut n = 2u32;
    loop {
        let numbered = format!("{dated}-{n}");
        if seen.insert(numbered.clone()) {
            return numbered;
        }
        n += 1;
    }
}

/// Parse org text into the events overlapping `[win_start, win_end)`,
/// expanding repeaters into the window.
///
/// Each entry (a headline and the lines under it, up to the next headline) is
/// read WHOLE before any of its events are emitted: properties first, then
/// stamps. That is what makes a headline's identity and properties independent
/// of where its drawer sits relative to its timestamp — the two orderings Emacs
/// produces used to yield different uids and lose the join link entirely, since
/// an event was emitted the instant a stamp was seen, from whatever properties
/// had been read so far.
pub fn agenda_events(
    org: &str,
    source: &str,
    win_start: NaiveDate,
    win_end: NaiveDate,
) -> Vec<OrgEvent> {
    let mut events = Vec::new();
    let lines: Vec<&str> = org.lines().collect();
    let mut at = 0;
    while at < lines.len() {
        let head = headline(lines[at]);
        if matches!(head, Head::Not) {
            at += 1; // preamble before the first headline carries no events
            continue;
        }
        let body = at + 1;
        let mut next = body;
        while next < lines.len() && matches!(headline(lines[next]), Head::Not) {
            next += 1;
        }
        if let Head::Title(title) = head {
            entry_events(
                title,
                &lines[body..next],
                source,
                win_start,
                win_end,
                &mut events,
            );
        }
        at = next;
    }
    events.sort_by(|a, b| a.start.cmp(&b.start));
    events
}

/// Emit one entry's events: its properties gathered from the whole entry, then
/// every stamp in it expanded into the window.
fn entry_events(
    title: &str,
    lines: &[&str],
    source: &str,
    win_start: NaiveDate,
    win_end: NaiveDate,
    events: &mut Vec<OrgEvent>,
) {
    let props = collect_props(lines);
    let mut seen: HashSet<String> = HashSet::new();
    for stamp in entry_stamps(lines) {
        let base_uid = props
            .org_id
            .clone()
            .unwrap_or_else(|| format!("org-{:016x}", fnv1a(&format!("{title}|{}", stamp.raw))));
        {
            // occurrences: the base date, then repeater steps into the window.
            // A range keeps its duration across occurrences, and overlaps the
            // window whenever its END does — a stay that started before the
            // window still spans into it.
            let span = stamp.end_date.map(|end| end - stamp.date);
            let mut date = stamp.date;
            let mut hops = 0u32;
            while date < win_end && hops < 1000 {
                let occ_end = span.map(|days| date + days);
                if date >= win_start || occ_end.is_some_and(|end| end >= win_start) {
                    let (start, end, all_day) = match (stamp.time, occ_end) {
                        // Timed on both sides: one continuous block.
                        (Some((from, _)), Some(end_date)) if stamp.end_time.is_some() => (
                            rfc3339(date, from),
                            rfc3339(end_date, stamp.end_time.expect("checked")),
                            false,
                        ),
                        // A range missing a time on either side degrades to
                        // all-day spanning (dtend exclusive, matching the
                        // single-day all-day convention).
                        (_, Some(end_date)) => {
                            let midnight = NaiveTime::from_hms_opt(0, 0, 0).expect("midnight");
                            (
                                rfc3339(date, midnight),
                                rfc3339(end_date + Duration::days(1), midnight),
                                true,
                            )
                        }
                        (Some((from, to)), None) => {
                            let until = to.unwrap_or_else(|| {
                                (NaiveDateTime::new(date, from) + Duration::hours(1)).time()
                            });
                            // An end at or before the start crosses midnight: org's
                            // `<… 20:30-00:30>` means 00:30 the NEXT day (as does the
                            // +1h default on a 23:45 start). EventKit rejects
                            // end<=start, and the org meaning is unambiguous.
                            let end_date = if until <= from {
                                date + Duration::days(1)
                            } else {
                                date
                            };
                            (rfc3339(date, from), rfc3339(end_date, until), false)
                        }
                        (None, None) => {
                            let midnight = NaiveTime::from_hms_opt(0, 0, 0).expect("midnight");
                            (
                                rfc3339(date, midnight),
                                rfc3339(date + Duration::days(1), midnight),
                                true,
                            )
                        }
                    };
                    let uid = if stamp.repeat.is_some() {
                        format!("{base_uid}-{date}")
                    } else {
                        base_uid.clone()
                    };
                    events.push(OrgEvent {
                        uid: unique_uid(uid, date, &mut seen),
                        title: title.to_string(),
                        source: source.to_string(),
                        start,
                        end,
                        all_day,
                        location: props.location.clone(),
                        url: props.url.clone(),
                        passcode: props.passcode.clone(),
                        alerts: props.alerts.clone(),
                    });
                }
                let Some(repeat) = stamp.repeat else { break };
                date = step(date, repeat);
                hops += 1;
            }
        }
    }
}

// ---- the faces -----------------------------------------------------------------

fn format_detail(label: &str, events: &[OrgEvent]) -> String {
    if events.is_empty() {
        return format!("org agenda — {label}\n\n  (no events)\n");
    }
    let mut out = format!("org agenda — {label}\n\n");
    for e in events {
        let date = e.start.split_once('T').map(|(d, _)| d).unwrap_or(&e.start);
        let when = if e.all_day {
            // dtend is exclusive; a span longer than one day shows its
            // inclusive range so a stay doesn't read as its first day.
            let last = e
                .end
                .split_once('T')
                .and_then(|(d, _)| d.parse::<NaiveDate>().ok())
                .map(|d| d - Duration::days(1));
            match last {
                Some(last) if last.to_string() != *date => {
                    format!("{date}..{last}  all-day")
                }
                _ => format!("{date}  all-day    "),
            }
        } else {
            let hhmm = |s: &str| {
                s.split_once('T')
                    .map(|(_, t)| t[..5.min(t.len())].to_string())
                    .unwrap_or_default()
            };
            format!("{date}  {}-{}", hhmm(&e.start), hhmm(&e.end))
        };
        out.push_str(&format!("  {when}  {}  [{}]", e.title, e.source));
        if let Some(location) = &e.location {
            out.push_str(&format!("  @ {location}"));
        }
        out.push('\n');
    }
    out
}

/// The skolemized Turtle event graph — same vocabulary as
/// `urn:personal:calendar as=text/turtle`, so the two union/diff as graphs.
fn format_turtle(events: &[OrgEvent]) -> String {
    let mut ttl = String::from(
        "@prefix ical: <http://www.w3.org/2002/12/cal/ical#> .\n\
         @prefix ik: <https://ikigai-rs.dev/ns#> .\n",
    );
    for e in events {
        let mut props = vec![
            "a ical:Vevent".to_string(),
            format!("ical:uid {}", ttl_str(&e.uid)),
            format!("ical:summary {}", ttl_str(&e.title)),
            format!("ical:dtstart {}", ttl_str(&e.start)),
            format!("ical:dtend {}", ttl_str(&e.end)),
            format!("ik:calendar {}", ttl_str(&e.source)),
        ];
        if e.all_day {
            props.push("ik:allDay true".to_string());
        }
        for minutes in &e.alerts {
            props.push(format!("ik:alert {minutes}"));
        }
        if let Some(location) = &e.location {
            props.push(format!("ical:location {}", ttl_str(location)));
        }
        // The :URL: link (and the :ZOOM_PASSCODE: beside it) deliberately emits
        // as ical:description, NOT ical:url. The derived view stores the link in
        // EKEvent .notes (its .url field is the urn:event:{uid} identity token),
        // and .notes reads back as ical:description — the convergence diff needs
        // the SAME predicate on both sides, or every linked event
        // delete-recreates on every pass (the documented infinite-loop class
        // this calendar has already hit). OrgEvent::description states why the
        // rendering must stay on one line.
        if let Some(description) = e.description() {
            props.push(format!("ical:description {}", ttl_str(&description)));
        }
        ttl.push_str(&format!(
            "\n<urn:event:{}> {} .\n",
            e.uid.replace(['<', '>', ' '], "-"),
            props.join(" ;\n    ")
        ));
    }
    ttl
}

fn ttl_str(s: &str) -> String {
    format!(
        "\"{}\"",
        s.replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('\n', " ")
    )
}

// ---- the endpoint ----------------------------------------------------------------

/// `urn:org:agenda[:{period}]` — the org agenda for a period (default `week`),
/// sourced through the kernel from the configured org-file resources.
pub struct AgendaEndpoint {
    /// The org files as kernel IRIs (e.g. `urn:orgfile:calendar.org`).
    files: Vec<String>,
}

#[async_trait::async_trait]
impl Endpoint for AgendaEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if self.files.is_empty() {
            return Err(Error::Endpoint(
                "urn:org:agenda: no org files configured — add org_dir + org_files to \
                 ~/.config/ikigai/calendar.json"
                    .to_string(),
            ));
        }
        let period = inv
            .bindings
            .get("period")
            .map(str::to_string)
            .unwrap_or_else(|| "week".to_string());
        let (win_start, win_end, label) = period_range(&period, Local::now().date_naive())?;

        let mut events = Vec::new();
        for file in &self.files {
            let iri = Iri::parse(file.as_str()).map_err(|e| {
                Error::Endpoint(format!("urn:org:agenda: bad file IRI {file}: {e}"))
            })?;
            // Through the kernel: capability-gated, and a dependency of this
            // result (golden threads propagate when the file space is cacheable).
            let repr = inv.source(&iri).await?;
            let text = String::from_utf8(repr.bytes)
                .map_err(|_| Error::Endpoint(format!("urn:org:agenda: {file} is not UTF-8")))?;
            let short = file.rsplit([':', '/']).next().unwrap_or(file).to_string();
            events.extend(agenda_events(&text, &short, win_start, win_end));
        }
        events.sort_by(|a, b| a.start.cmp(&b.start));

        // q= — case-insensitive title+location search (mirrors urn:personal:calendar).
        let mut label = label;
        if let Ok(q) = inv.inline_str("q") {
            let needle = q.to_lowercase();
            events.retain(|e| {
                e.title.to_lowercase().contains(&needle)
                    || e.location
                        .as_deref()
                        .is_some_and(|l| l.to_lowercase().contains(&needle))
            });
            label = format!("{label} · matching \"{q}\"");
        }

        let want_turtle = inv
            .inline_str("as")
            .map(|s| s.contains("turtle"))
            .unwrap_or(false);
        if want_turtle {
            return Ok(Representation::new(
                ReprType::new("text/turtle").with_param("charset", "utf-8"),
                format_turtle(&events).into_bytes(),
            ));
        }
        Ok(Representation::new(
            ReprType::new("text/plain").with_param("charset", "utf-8"),
            format_detail(&label, &events).into_bytes(),
        ))
    }

    fn name(&self) -> &str {
        "org-agenda"
    }

    fn describe(&self) -> Description {
        Description::new("org-agenda")
            .title("Org agenda")
            .summary(
                "Date-fixed events from the configured org files for a period \
                 (urn:org:agenda:{period}: today, tomorrow, week, month, a month name, \
                 YYYY-MM, YYYY-MM-DD; bare = week), repeaters expanded. as=text/turtle \
                 renders the same skolemized event graph as urn:personal:calendar.",
            )
            .verb(Verb::Source)
            .verb(Verb::Meta)
            .input(
                ArgSpec::new("period")
                    .summary("the time window, captured from the IRI (default: week)")
                    .binding(),
            )
            .input(
                ArgSpec::new("as")
                    .summary("text/turtle for the skolemized event graph")
                    .optional(),
            )
            .input(
                ArgSpec::new("q")
                    .summary("search: case-insensitive match over title + location")
                    .optional(),
            )
            .output("text/plain;charset=utf-8")
    }
}

/// Mount the agenda: `urn:org:agenda` and `urn:org:agenda:{period}`, reading
/// the given org-file resources through the kernel.
pub fn space(files: Vec<String>) -> EndpointSpace {
    EndpointSpace::new()
        .bind(
            Exact::new("urn:org:agenda"),
            AgendaEndpoint {
                files: files.clone(),
            },
        )
        .bind(
            UriTemplate::parse("urn:org:agenda:{period}").expect("valid template"),
            AgendaEndpoint { files },
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ORG: &str = "\
#+TITLE: Calendar

# a comment with a fake stamp <2026-01-01 Thu> that must be ignored

* Dinner with the Hendersons
  <2026-07-11 Sat 19:00-21:00>

* Trash & recycling out
  <2026-07-03 Fri 07:00 +1w>

* Anniversary
  <2026-08-15 Sat +1y>

* Dentist — cleaning
  :ID: dentist-2026-07
  :ALERT: 1h 1d
  <2026-07-22 Wed 10:30-11:15>

* Planning call
  :ID: planning-call-1
  :LOCATION: Microsoft Teams Meeting
  :URL: https://teams.microsoft.com/l/meetup-join/abc
  <2026-07-15 Wed 09:00-10:00>

* Conference in Berlin
  <2026-07-14 Tue>--<2026-07-17 Fri>

* Overnight shift
  <2026-07-20 Mon 22:00>--<2026-07-21 Tue 06:00>

* TODO Move the boxes
  <2026-07-24 Fri>

* DONE Return the library books
  <2026-07-08 Wed>

* CANCELLED Coffee with Dave
  <2026-07-09 Thu 09:00>
";

    fn july() -> (NaiveDate, NaiveDate) {
        (
            NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
        )
    }

    #[test]
    fn a_time_range_crossing_midnight_ends_the_next_day() {
        // <Mon 20:30-00:30> — org's single-stamp form for a class running past
        // midnight (found live: the India-cohort teaching block). End <= start
        // means next day, never an inverted event.
        let org = "* Night class\n  <2026-08-31 Mon 20:30-00:30>\n\n* Late cap\n  <2026-08-31 Mon 23:45>\n";
        let events = agenda_events(
            org,
            "t.org",
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 10, 1).unwrap(),
        );
        let class = events.iter().find(|e| e.title == "Night class").unwrap();
        assert!(class.start.starts_with("2026-08-31T20:30"));
        assert!(class.end.starts_with("2026-09-01T00:30"), "{}", class.end);
        // the +1h default wrapping midnight gets the same treatment
        let cap = events.iter().find(|e| e.title == "Late cap").unwrap();
        assert!(cap.start.starts_with("2026-08-31T23:45"));
        assert!(cap.end.starts_with("2026-09-01T00:45"), "{}", cap.end);
    }

    #[test]
    fn todo_states_map_to_calendar_semantics() {
        let (start, end) = july();
        let events = agenda_events(ORG, "calendar.org", start, end);
        assert!(
            events.iter().any(|e| e.title == "TODO Move the boxes"),
            "an OPEN todo stays, keyword and all — the reminder is wanted"
        );
        assert!(
            events.iter().any(|e| e.title == "Return the library books"),
            "DONE keeps the event under its clean name"
        );
        assert!(
            !events.iter().any(|e| e.title.contains("DONE")),
            "…but the keyword itself never reaches the calendar"
        );
        assert!(
            !events.iter().any(|e| e.title.contains("Dave")),
            "CANCELLED events leave the calendar"
        );
    }

    #[test]
    fn a_date_range_is_one_spanning_event() {
        let (start, end) = july();
        let events = agenda_events(ORG, "calendar.org", start, end);
        let conf: Vec<_> = events
            .iter()
            .filter(|e| e.title.contains("Berlin"))
            .collect();
        assert_eq!(conf.len(), 1, "one event, not one per stamp");
        assert!(conf[0].all_day);
        assert!(conf[0].start.starts_with("2026-07-14T00:00"));
        assert!(
            conf[0].end.starts_with("2026-07-18T00:00"),
            "dtend exclusive: day after the inclusive end"
        );
        let detail = format_detail("july", &events);
        assert!(
            detail.contains("2026-07-14..2026-07-17  all-day"),
            "detail shows the inclusive range: {detail}"
        );
    }

    #[test]
    fn a_timed_range_is_one_continuous_block() {
        let (start, end) = july();
        let events = agenda_events(ORG, "calendar.org", start, end);
        let shift = events
            .iter()
            .find(|e| e.title.contains("Overnight"))
            .unwrap();
        assert!(!shift.all_day);
        assert!(shift.start.starts_with("2026-07-20T22:00"));
        assert!(shift.end.starts_with("2026-07-21T06:00"));
    }

    #[test]
    fn a_range_straddling_the_window_start_is_kept() {
        // Window opens mid-conference: the stay must still appear.
        let start = NaiveDate::from_ymd_opt(2026, 7, 16).unwrap();
        let end = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
        let events = agenda_events(ORG, "calendar.org", start, end);
        assert!(
            events.iter().any(|e| e.title.contains("Berlin")),
            "started 07-14, window opens 07-16 — still spanning"
        );
    }

    #[test]
    fn parses_timed_events() {
        let (start, end) = july();
        let events = agenda_events(ORG, "calendar.org", start, end);
        let dinner = events
            .iter()
            .find(|e| e.title.contains("Hendersons"))
            .unwrap();
        assert!(dinner.start.starts_with("2026-07-11T19:00"));
        assert!(dinner.end.starts_with("2026-07-11T21:00"));
        assert!(!dinner.all_day);
    }

    #[test]
    fn repeaters_expand_into_the_window() {
        let (start, end) = july();
        let events = agenda_events(ORG, "calendar.org", start, end);
        let trash: Vec<_> = events
            .iter()
            .filter(|e| e.title.contains("Trash"))
            .collect();
        // Jul 3, 10, 17, 24, 31 — five Fridays
        assert_eq!(trash.len(), 5, "{trash:?}");
        assert!(trash[0].start.starts_with("2026-07-03T07:00"));
        assert!(trash[4].start.starts_with("2026-07-31T07:00"));
        // occurrence uids are distinct and date-suffixed
        assert_ne!(trash[0].uid, trash[1].uid);
        assert!(trash[1].uid.ends_with("2026-07-10"));
        // a timed stamp without an end defaults to one hour
        assert!(trash[0].end.starts_with("2026-07-03T08:00"));
    }

    #[test]
    fn yearly_repeater_and_all_day() {
        let events = agenda_events(
            ORG,
            "calendar.org",
            NaiveDate::from_ymd_opt(2027, 8, 1).unwrap(),
            NaiveDate::from_ymd_opt(2027, 9, 1).unwrap(),
        );
        let anniversary = events.iter().find(|e| e.title == "Anniversary").unwrap();
        assert!(anniversary.start.starts_with("2027-08-15"));
        assert!(anniversary.all_day);
    }

    #[test]
    fn an_org_id_wins_as_the_uid_and_comments_are_ignored() {
        let (start, end) = july();
        let events = agenda_events(ORG, "calendar.org", start, end);
        let dentist = events.iter().find(|e| e.title.contains("Dentist")).unwrap();
        assert_eq!(dentist.uid, "dentist-2026-07");
        assert!(
            !events.iter().any(|e| e.start.starts_with("2026-01-01")),
            "the comment's stamp must not become an event"
        );
    }

    #[test]
    fn events_outside_the_window_are_excluded() {
        let (start, end) = july();
        let events = agenda_events(ORG, "calendar.org", start, end);
        assert!(
            !events.iter().any(|e| e.title == "Anniversary"),
            "Aug 15 is outside July"
        );
    }

    #[test]
    fn alerts_parse_and_emit() {
        assert_eq!(parse_alerts("1h 1d"), vec![60, 1440]);
        assert_eq!(parse_alerts("30m,45"), vec![30, 45]);
        assert_eq!(parse_alerts("junk 2h"), vec![120]);
        let (start, end) = july();
        let events = agenda_events(ORG, "calendar.org", start, end);
        let dentist = events.iter().find(|e| e.title.contains("Dentist")).unwrap();
        assert_eq!(dentist.alerts, vec![60, 1440]);
        let dinner = events
            .iter()
            .find(|e| e.title.contains("Hendersons"))
            .unwrap();
        assert!(dinner.alerts.is_empty(), "no :ALERT: -> no alarms");
        let ttl = format_turtle(&events);
        assert!(ttl.contains("ik:alert 60"));
        assert!(ttl.contains("ik:alert 1440"));
    }

    #[test]
    fn location_and_url_drawers_are_parsed_and_reset_per_headline() {
        let (start, end) = july();
        let events = agenda_events(ORG, "calendar.org", start, end);
        let call = events.iter().find(|e| e.title == "Planning call").unwrap();
        assert_eq!(call.location.as_deref(), Some("Microsoft Teams Meeting"));
        assert_eq!(
            call.url.as_deref(),
            Some("https://teams.microsoft.com/l/meetup-join/abc")
        );
        // The drawers belong to their heading only — the next entries carry none.
        let berlin = events.iter().find(|e| e.title.contains("Berlin")).unwrap();
        assert_eq!(berlin.location, None);
        assert_eq!(berlin.url, None);
        // The detail face shows the location, like the calendar side.
        let detail = format_detail("july", &events);
        assert!(
            detail.contains("Planning call  [calendar.org]  @ Microsoft Teams Meeting"),
            "{detail}"
        );
    }

    #[test]
    fn the_url_drawer_emits_as_ical_description_never_ical_url() {
        // The derived view stores the link in EKEvent .notes, which reads back
        // as ical:description — emitting ical:url here would put a predicate in
        // the desired graph the view can never echo, and the derive would
        // delete-recreate every linked event forever.
        let (start, end) = july();
        let events = agenda_events(ORG, "calendar.org", start, end);
        let ttl = format_turtle(&events);
        assert!(ttl.contains("ical:location \"Microsoft Teams Meeting\""));
        assert!(
            ttl.contains("ical:description \"https://teams.microsoft.com/l/meetup-join/abc\""),
            "{ttl}"
        );
        assert!(
            !ttl.contains("ical:url"),
            "the link must ride the predicate the view reads back: {ttl}"
        );
    }

    /// The two shapes Emacs actually writes, same entry both ways: the drawer
    /// ABOVE the stamp, and a `SCHEDULED:` line above the drawer (indented and
    /// flush-left, which is how the live file has them).
    const BOTH_ORDERS: &str = "\
* Call: Rita Fernando
  :PROPERTIES:
  :ID: 76F47687-B4DF-4FB8-ADDC-9A6A85ED12A1
  :ATTENDEES: rfernando@oreilly.com
  :URL:      https://us06web.zoom.us/j/88460877532
  :ZOOM_PASSCODE: 335168
  :END:
  <2026-08-25 Tue 11:00-11:30>

* Chat with Kevin
SCHEDULED: <2026-08-25 Tue 11:00-11:30>
:PROPERTIES:
:ATTENDEES: Kevin.mcgorry@vybright.com
:URL:      https://us06web.zoom.us/j/88460877532
:ZOOM_PASSCODE: 335168
:ID:       76F47687-B4DF-4FB8-ADDC-9A6A85ED12A1
:END:
";

    fn august() -> (NaiveDate, NaiveDate) {
        (
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
        )
    }

    #[test]
    fn a_drawer_speaks_for_its_entry_above_or_below_the_stamp() {
        // THE BUG: properties used to apply only if they had already been read
        // when the stamp was reached, so `SCHEDULED:`-first entries silently lost
        // their `:ID:` (falling back to a title|stamp hash) and their join link.
        // Same entry, two orderings, one result — everything but the title.
        let (start, end) = august();
        let events = agenda_events(BOTH_ORDERS, "calendar.org", start, end);
        assert_eq!(events.len(), 2, "{events:#?}");
        let rita = events.iter().find(|e| e.title.contains("Rita")).unwrap();
        let kevin = events.iter().find(|e| e.title.contains("Kevin")).unwrap();
        assert_eq!(kevin.uid, "76F47687-B4DF-4FB8-ADDC-9A6A85ED12A1");
        assert_eq!(kevin.uid, rita.uid);
        assert_eq!(kevin.url, rita.url);
        assert_eq!(
            kevin.url.as_deref(),
            Some("https://us06web.zoom.us/j/88460877532")
        );
        assert_eq!(kevin.passcode, rita.passcode);
        assert_eq!(kevin.start, rita.start);
        assert_eq!(kevin.end, rita.end);
        assert_eq!(kevin.description(), rita.description());
        assert!(
            !kevin.uid.starts_with("org-"),
            "an explicit :ID: below the stamp must still win over the hash"
        );
    }

    #[test]
    fn a_property_in_the_body_cannot_seize_the_entry() {
        // An entry's body can be captured invite text (the ingest side pastes a
        // source event's notes there). A `:ID:` in that text must not become the
        // entry's identity, and must not reach a stamp later in the body either.
        let org = "\
* Meeting
  :PROPERTIES:
  :ID: real-id
  :END:
  <2026-08-20 Thu 09:00-10:00>
  Notes from the invite follow.
  :ID: hijacked
  :URL: https://evil.example/join
  Reschedule proposed for <2026-08-21 Fri 09:00-10:00>.
";
        let (start, end) = august();
        let events = agenda_events(org, "calendar.org", start, end);
        assert_eq!(events.len(), 2, "both stamps are the entry's: {events:#?}");
        assert!(
            events.iter().all(|e| e.uid.starts_with("real-id")),
            "body :ID: ignored: {events:#?}"
        );
        assert!(
            events.iter().all(|e| e.url.is_none()),
            "body :URL: ignored: {events:#?}"
        );
    }

    #[test]
    fn two_stamps_under_one_id_get_distinct_subjects() {
        // Both stamps are the entry's, so both would take the :ID: as their uid —
        // one `urn:event:` subject emitted twice is a merged, self-contradicting
        // event the deriver can never converge on. The first keeps the bare id.
        let org = "\
* Two sittings
  :PROPERTIES:
  :ID: exam-2026
  :END:
  <2026-08-20 Thu 09:00-10:00>
  <2026-08-21 Fri 09:00-10:00>
";
        let (start, end) = august();
        let events = agenda_events(org, "calendar.org", start, end);
        let uids: Vec<&str> = events.iter().map(|e| e.uid.as_str()).collect();
        assert_eq!(
            uids,
            vec!["exam-2026", "exam-2026-2026-08-21"],
            "{events:#?}"
        );
    }

    #[test]
    fn a_drawerless_alert_below_end_is_still_the_entrys() {
        // The live file's shape: `:ALERT:` sits AFTER `:END:` and above the
        // stamp — drawer-less, but still the entry's preamble.
        let org = "\
* Development Kick-off
  :PROPERTIES:
  :ID: 6c5vgfbkk9jhr9ct7ee35ggoan@google.com
  :END:
  :ALERT: 10m
  <2026-08-20 Thu 09:00-10:00>
";
        let (start, end) = august();
        let events = agenda_events(org, "calendar.org", start, end);
        assert_eq!(events[0].alerts, vec![10], "{events:#?}");
        assert_eq!(events[0].uid, "6c5vgfbkk9jhr9ct7ee35ggoan@google.com");
    }

    #[test]
    fn the_passcode_rides_the_description_beside_the_link() {
        let (start, end) = august();
        let events = agenda_events(BOTH_ORDERS, "calendar.org", start, end);
        let rita = events.iter().find(|e| e.title.contains("Rita")).unwrap();
        assert_eq!(rita.passcode.as_deref(), Some("335168"));
        assert_eq!(
            rita.description().as_deref(),
            Some("https://us06web.zoom.us/j/88460877532 | Passcode: 335168")
        );
        let ttl = format_turtle(&events);
        assert!(
            ttl.contains(
                "ical:description \"https://us06web.zoom.us/j/88460877532 | Passcode: 335168\""
            ),
            "{ttl}"
        );
    }

    #[test]
    fn a_link_without_a_passcode_renders_exactly_as_before() {
        // Adding the passcode must not restate the events that already carry a
        // link: a changed description would delete-recreate every one of them.
        let (start, end) = july();
        let events = agenda_events(ORG, "calendar.org", start, end);
        let call = events.iter().find(|e| e.title == "Planning call").unwrap();
        assert_eq!(call.passcode, None);
        assert_eq!(
            call.description().as_deref(),
            Some("https://teams.microsoft.com/l/meetup-join/abc"),
            "byte-identical to the 0.1.5 rendering"
        );
    }

    #[test]
    fn the_description_round_trips_byte_identically() {
        // The description travels org -> Turtle -> EKEvent .notes -> read back ->
        // Turtle, and the deriver compares those graphs BYTE-WISE. The two
        // serializers agree on every character EXCEPT the line breaks: this crate
        // flattens `\n` to a space, ikigai-personal escapes it as `\n`, so one
        // newline anywhere means the two sides never render the same string and
        // every linked event delete-recreates forever. A description carrying no
        // `\r`/`\n` is invariant under BOTH, which is what makes the trip an
        // identity — assert the property rather than trusting the format.
        let (start, end) = august();
        let mut events = agenda_events(BOTH_ORDERS, "calendar.org", start, end);
        events.extend(agenda_events(ORG, "calendar.org", july().0, july().1));
        let mut checked = 0;
        for description in events.iter().filter_map(OrgEvent::description) {
            assert!(
                !description.contains(['\n', '\r']),
                "single line, or the two serializers disagree: {description:?}"
            );
            // This crate's serializer: newline-flattening is the ONLY lossy step,
            // so a single-line string comes back out of the literal unchanged.
            assert_eq!(ttl_str(&description), format!("\"{description}\""));
            // ikigai-personal's serializer, applied to the same string: it drops
            // `\r` and escapes `\n`, and agrees character-for-character here.
            let theirs = description
                .replace('\\', "\\\\")
                .replace('"', "\\\"")
                .replace('\r', "")
                .replace('\n', "\\n");
            assert_eq!(format!("\"{theirs}\""), ttl_str(&description));
            // EKEvent .notes stores the string verbatim, so the value the view
            // reads back is this one — and the ingest side recovers the join link
            // by taking the first URL token, which the rendering keeps first.
            if let Some(url) = events
                .iter()
                .find(|e| e.description().as_deref() == Some(description.as_str()))
                .and_then(|e| e.url.clone())
            {
                assert_eq!(description.split_whitespace().next(), Some(url.as_str()));
            }
            checked += 1;
        }
        assert!(checked >= 3, "the fixtures must exercise this: {checked}");
    }

    #[test]
    fn turtle_matches_the_calendar_vocabulary() {
        let (start, end) = july();
        let events = agenda_events(ORG, "calendar.org", start, end);
        let ttl = format_turtle(&events);
        assert!(ttl.contains("a ical:Vevent"));
        assert!(ttl.contains("<urn:event:dentist-2026-07>"));
        assert!(ttl.contains("ik:calendar \"calendar.org\""));
        assert!(!ttl.contains("_:"), "skolemized — no blank nodes");
    }

    #[test]
    fn year_period_spans_the_calendar_year() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 2).unwrap();
        let (start, end, label) = period_range("year", today).unwrap();
        assert_eq!(start, NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        assert_eq!(end, NaiveDate::from_ymd_opt(2027, 1, 1).unwrap());
        assert_eq!(label, "2026");
    }

    #[test]
    fn range_periods_are_end_inclusive() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 2).unwrap();
        let (start, end, label) = period_range("2026-07-01..2026-12-31", today).unwrap();
        assert_eq!(label, "2026-07-01..2026-12-31");
        assert_eq!(end - start, chrono::Duration::days(184));
        assert!(period_range("2026-12-31..2026-07-01", today).is_err());
    }

    #[test]
    fn period_grammar_mirrors_the_calendar() {
        let today = NaiveDate::from_ymd_opt(2026, 7, 2).unwrap();
        assert!(period_range("week", today)
            .unwrap()
            .2
            .contains("2026-06-29"));
        assert_eq!(period_range("month", today).unwrap().2, "2026-07");
        assert!(period_range("fortnight", today).is_err());
    }
}

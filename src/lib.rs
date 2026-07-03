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
//! timestamps are ignored. Untimed stamps are all-day; a timed stamp without
//! an end defaults to one hour. Repeaters (`+1w`, `+1y`, …) are expanded into
//! the requested window.

use chrono::{Datelike, Duration, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use ikigai_core::{
    ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation, Iri, ReprType,
    Representation, Result, UriTemplate, Verb,
};

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
            if let Ok(date) = other.parse::<NaiveDate>() {
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
         a month name, YYYY-MM, or YYYY-MM-DD"
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
        raw: inner.trim().to_string(),
    })
}

/// Every `<…>` active timestamp in a line (inactive `[…]` ignored).
fn stamps_in(line: &str) -> Vec<Stamp> {
    let mut found = Vec::new();
    let mut rest = line;
    while let Some(open) = rest.find('<') {
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        if let Some(stamp) = parse_stamp(&rest[open + 1..open + close]) {
            found.push(stamp);
        }
        rest = &rest[open + close + 1..];
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

/// Parse org text into the events overlapping `[win_start, win_end)`,
/// expanding repeaters into the window.
pub fn agenda_events(
    org: &str,
    source: &str,
    win_start: NaiveDate,
    win_end: NaiveDate,
) -> Vec<OrgEvent> {
    let mut events = Vec::new();
    let mut headline: Option<String> = None;
    let mut org_id: Option<String> = None;
    for line in org.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed
            .strip_prefix('*')
            .filter(|_| trimmed.starts_with('*'))
        {
            // a headline: any number of stars then a space
            let rest = rest.trim_start_matches('*');
            if let Some(title) = rest.strip_prefix(' ') {
                headline = Some(title.trim().to_string());
                org_id = None;
                continue;
            }
        }
        if trimmed.starts_with("# ") || trimmed.starts_with("#+") {
            continue; // comments / directives never carry agenda stamps
        }
        if let Some(id) = trimmed.strip_prefix(":ID:") {
            org_id = Some(id.trim().to_string());
            continue;
        }
        let Some(title) = &headline else { continue };
        for stamp in stamps_in(line) {
            let base_uid = org_id.clone().unwrap_or_else(|| {
                format!("org-{:016x}", fnv1a(&format!("{title}|{}", stamp.raw)))
            });
            // occurrences: the base date, then repeater steps into the window
            let mut date = stamp.date;
            let mut hops = 0u32;
            while date < win_end && hops < 1000 {
                if date >= win_start {
                    let (start, end, all_day) = match stamp.time {
                        Some((from, to)) => {
                            let until = to.unwrap_or_else(|| {
                                (NaiveDateTime::new(date, from) + Duration::hours(1)).time()
                            });
                            (rfc3339(date, from), rfc3339(date, until), false)
                        }
                        None => {
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
                        uid,
                        title: title.clone(),
                        source: source.to_string(),
                        start,
                        end,
                        all_day,
                    });
                }
                let Some(repeat) = stamp.repeat else { break };
                date = step(date, repeat);
                hops += 1;
            }
        }
    }
    events.sort_by(|a, b| a.start.cmp(&b.start));
    events
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
            format!("{date}  all-day    ")
        } else {
            let hhmm = |s: &str| {
                s.split_once('T')
                    .map(|(_, t)| t[..5.min(t.len())].to_string())
                    .unwrap_or_default()
            };
            format!("{date}  {}-{}", hhmm(&e.start), hhmm(&e.end))
        };
        out.push_str(&format!("  {when}  {}  [{}]\n", e.title, e.source));
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

        // q= — case-insensitive title search (org events carry no location).
        let mut label = label;
        if let Ok(q) = inv.inline_str("q") {
            let needle = q.to_lowercase();
            events.retain(|e| e.title.to_lowercase().contains(&needle));
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
                    .summary("search: case-insensitive match over event titles")
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
  <2026-07-22 Wed 10:30-11:15>
";

    fn july() -> (NaiveDate, NaiveDate) {
        (
            NaiveDate::from_ymd_opt(2026, 7, 1).unwrap(),
            NaiveDate::from_ymd_opt(2026, 8, 1).unwrap(),
        )
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

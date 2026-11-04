//! TODO entries as resources: read by id, state or tag, and appended without ever
//! losing an edit made in Emacs.
//!
//! An entry is any headline carrying a keyword from its file's own `#+TODO:` line
//! (default `TODO | DONE`). The org files stay the only state: every read here is a
//! pure function of them, read through the kernel, so it is cacheable under their
//! golden threads and live over a live file space.
//!
//! ## The syntax is Emacs's, stricter than the agenda's
//!
//! An entry's body holds links and src blocks, so a headline is a line starting
//! with stars at **column zero** (an indented `* x` is a list item), and an entry's
//! metadata is what org itself accepts: planning lines and drawers directly under
//! the headline, up to the first other line. A `:ID:` further down is body text and
//! can never seize the entry's identity.
//!
//! ## The write: compare and replace, or refuse
//!
//! [`todo_space`]'s Sink reads the file, builds the new text by splicing one entry
//! into the old bytes (never re-rendering what is there), and writes it with
//! `if-match` set to the content id of what it read. `ikigai-fs` compares and renames
//! under a lock and answers [`Error::Conflict`] if the file moved in between.
//!
//! A file space that ignores `if-match` would turn that into a blind overwrite, and
//! `ikigai-fs` before 0.4.1 is one. So the Sink first asks the kernel's
//! `urn:kernel:validate` whether the file's Sink declares `if-match`, and refuses
//! with [`Error::Unsupported`], having written nothing, when it does not.
//!
//! What the comparison cannot cover is stated in `ikigai-fs`: a writer outside that
//! crate (Emacs saving, Dropbox syncing) takes no lock, so one landing in the instant
//! between the comparison and the rename is still lost. The README says so too.

use ikigai_core::{
    ActionSpec, ArgRef, ArgSpec, Description, Endpoint, EndpointSpace, Error, Exact, Invocation,
    Iri, ListSeparator, ReprType, Representation, Request, Result, UriTemplate, Verb,
};
use std::collections::{HashMap, HashSet};

use crate::{fnv1a, property, single_line, XSD_STRING};

/// The capability `ikigai-fs` asks of a reader, which every read here passes on.
pub const FS_READ: &str = "urn:cap:fs:read:*";
/// The capability `ikigai-fs` asks of a writer.
pub const FS_WRITE: &str = "urn:cap:fs:write:*";

/// The column `org-tags-column`'s default right-aligns a headline's tags to.
const TAGS_COLUMN: usize = 77;

/// One org file a host binds: the `{name}` a Sink addresses it by, and the IRI its
/// file space serves it at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TodoFile {
    /// The name in `urn:org:todo:file:{name}`.
    pub name: String,
    /// The kernel IRI, e.g. `urn:orgfile:queue.org`.
    pub iri: String,
}

impl TodoFile {
    /// A bound file: its `{name}` and its IRI.
    pub fn new(name: impl Into<String>, iri: impl Into<String>) -> Self {
        TodoFile {
            name: name.into(),
            iri: iri.into(),
        }
    }
}

/// One TODO entry, read from its file's text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TodoEntry {
    /// The `:ID:` property, or a stable `org-…` FNV-1a id when it has none.
    pub id: String,
    /// The keyword, one of the file's own.
    pub state: String,
    /// The headline text, without keyword or tags.
    pub title: String,
    /// The headline's own tags, in order (no inheritance, no `#+FILETAGS:`).
    pub tags: Vec<String>,
    /// The `CLOSED:` timestamp's inside, as written: `2026-11-04 Wed 10:00`.
    pub closed: Option<String>,
    /// The `:LOGBOOK:` drawer's items, one line each, without `CLOCK:` lines.
    pub logbook: Vec<String>,
    /// What follows the metadata, verbatim, with blank edges removed.
    pub body: String,
    /// The bound name of the file it came from.
    pub file: String,
}

// ---- the keywords -----------------------------------------------------------------

/// A file's TODO keywords.
struct Keywords {
    open: Vec<String>,
    done: Vec<String>,
}

impl Keywords {
    /// The file's own: every `#+TODO:` line outside a block, or org's default.
    fn of(org: &str) -> Keywords {
        let mut keywords = Keywords {
            open: Vec::new(),
            done: Vec::new(),
        };
        let mut block = false;
        for line in org.lines() {
            if stars(line).is_some() {
                block = false; // a headline ends any block, in org as here
                continue;
            }
            let trimmed = line.trim_start().to_ascii_uppercase();
            if trimmed.starts_with("#+BEGIN_") {
                block = true;
            } else if trimmed.starts_with("#+END_") {
                block = false;
            } else if let (false, Some(words)) = (block, keyword_line(line)) {
                keywords.add_sequence(words);
            }
        }
        if keywords.open.is_empty() && keywords.done.is_empty() {
            keywords.add_sequence("TODO | DONE");
        }
        keywords
    }

    /// One sequence: `TODO(t) NEXT | DONE(d!)`. Fast-access and logging marks are
    /// dropped; with no `|`, org makes the last keyword the done one.
    fn add_sequence(&mut self, words: &str) {
        let word = |w: &str| w.split('(').next().unwrap_or(w).to_string();
        let tokens: Vec<&str> = words.split_whitespace().collect();
        match tokens.iter().position(|t| *t == "|") {
            Some(bar) => {
                self.open.extend(tokens[..bar].iter().map(|w| word(w)));
                self.done.extend(tokens[bar + 1..].iter().map(|w| word(w)));
            }
            None => {
                if let Some((last, rest)) = tokens.split_last() {
                    self.open.extend(rest.iter().map(|w| word(w)));
                    self.done.push(word(last));
                }
            }
        }
    }

    fn contains(&self, word: &str) -> bool {
        self.open.iter().chain(&self.done).any(|k| k == word)
    }

    /// Every keyword, for a refusal to list.
    fn listed(&self) -> String {
        let all: Vec<&str> = self
            .open
            .iter()
            .chain(&self.done)
            .map(String::as_str)
            .collect();
        all.join(", ")
    }
}

/// The value of a `#+TODO:` line (or its older spellings `#+SEQ_TODO:`, `#+TYP_TODO:`).
fn keyword_line(line: &str) -> Option<&str> {
    let (key, value) = line.trim_start().strip_prefix("#+")?.split_once(':')?;
    ["TODO", "SEQ_TODO", "TYP_TODO"]
        .iter()
        .any(|k| key.eq_ignore_ascii_case(k))
        .then_some(value)
}

// ---- the parser -------------------------------------------------------------------

/// A headline's level and the text after its stars: stars at column zero, then a space.
fn stars(line: &str) -> Option<(usize, &str)> {
    let rest = line.trim_start_matches('*');
    let level = line.len() - rest.len();
    (level > 0).then_some(())?;
    Some((level, rest.strip_prefix(' ')?))
}

/// What a headline's text holds.
struct Heading<'a> {
    state: Option<&'a str>,
    title: &'a str,
    tags: Vec<&'a str>,
}

/// Split a headline's text: a leading keyword of this file's, a trailing `:a:b:`.
fn heading<'a>(text: &'a str, keywords: &Keywords) -> Heading<'a> {
    let text = text.trim();
    let (state, rest) = match text.split_once(char::is_whitespace) {
        Some((first, rest)) if keywords.contains(first) => (Some(first), rest.trim_start()),
        None if keywords.contains(text) => (Some(text), ""),
        _ => (None, text),
    };
    let (before, last) = rest.rsplit_once(char::is_whitespace).unwrap_or(("", rest));
    match tag_group(last) {
        Some(tags) => Heading {
            state,
            title: before.trim_end(),
            tags,
        },
        None => Heading {
            state,
            title: rest,
            tags: Vec::new(),
        },
    }
}

/// The tags in a `:a:b:` token, or `None` when it is not one.
fn tag_group(token: &str) -> Option<Vec<&str>> {
    let inner = token.strip_prefix(':')?.strip_suffix(':')?;
    let tags: Vec<&str> = inner.split(':').collect();
    tags.iter().all(|t| is_tag(t)).then_some(tags)
}

/// Org's tag characters: letters, digits, `_`, `@`, `#` and `%`.
fn is_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag
            .chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '_' | '@' | '#' | '%'))
}

/// An entry's metadata and where its body starts.
#[derive(Default)]
struct Meta {
    id: Option<String>,
    closed: Option<String>,
    logbook: Vec<String>,
    body_start: usize,
}

/// Read what org reads as metadata: planning lines and drawers, directly under the
/// headline, up to the first other line. The first `:ID:` is the entry's. A drawer
/// with no `:END:` is not a drawer, in org as here, so it stays body text.
fn metadata(lines: &[&str]) -> Meta {
    let mut meta = Meta::default();
    let mut at = 0;
    while let Some(line) = lines.get(at) {
        let trimmed = line.trim();
        if let Some(found) = planning(trimmed) {
            meta.closed = meta.closed.or(found);
            at += 1;
            continue;
        }
        let Some((name, "")) = property(trimmed) else {
            break;
        };
        if name.eq_ignore_ascii_case("END") {
            break;
        }
        let Some(len) = lines[at + 1..]
            .iter()
            .position(|l| l.trim().eq_ignore_ascii_case(":END:"))
        else {
            break;
        };
        let drawer = &lines[at + 1..at + 1 + len];
        if name.eq_ignore_ascii_case("PROPERTIES") {
            for held in drawer {
                if let Some((key, value)) = property(held.trim()) {
                    if key.eq_ignore_ascii_case("ID") && meta.id.is_none() {
                        meta.id = single_line(value);
                    }
                }
            }
        } else if name.eq_ignore_ascii_case("LOGBOOK") {
            meta.logbook = logbook_items(drawer);
        }
        at += len + 2;
    }
    meta.body_start = at;
    meta
}

/// A planning line's `CLOSED:` stamp: `Some(None)` for a planning line without one,
/// `None` for any other line.
fn planning(trimmed: &str) -> Option<Option<String>> {
    if !["CLOSED:", "SCHEDULED:", "DEADLINE:"]
        .iter()
        .any(|k| trimmed.starts_with(k))
    {
        return None;
    }
    let stamp = trimmed.split_once("CLOSED:").and_then(|(_, rest)| {
        let inner = rest.trim_start().strip_prefix('[')?;
        Some(inner.split_once(']')?.0.trim().to_string())
    });
    Some(stamp)
}

/// A `:LOGBOOK:` drawer's `- ` items, each one line with its spacing collapsed and
/// org's `\\` line break removed. A `CLOCK:` line is not a note, and ends one.
fn logbook_items(drawer: &[&str]) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();
    let mut open = false;
    for line in drawer {
        let trimmed = line.trim();
        let (text, continues) = match trimmed.strip_prefix("- ") {
            Some(text) => (text, false),
            None if open && line.starts_with(char::is_whitespace) => (trimmed, true),
            None => {
                open = false;
                continue;
            }
        };
        let text = text.trim_end_matches('\\').split_whitespace();
        let text = text.collect::<Vec<_>>().join(" ");
        match items.last_mut() {
            Some(last) if continues => {
                if !text.is_empty() {
                    last.push(' ');
                    last.push_str(&text);
                }
            }
            _ => items.push(text),
        }
        open = true;
    }
    items
}

/// One headline of a file, whether or not it carries a keyword.
struct Section<'a> {
    heading: Heading<'a>,
    meta: Meta,
    lines: Vec<&'a str>,
}

/// Every headline of a file, each with the lines up to the next headline.
fn sections<'a>(org: &'a str, keywords: &Keywords) -> Vec<Section<'a>> {
    let lines: Vec<&str> = org.lines().collect();
    let mut found = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        let Some((_, text)) = stars(lines[at]) else {
            at += 1;
            continue;
        };
        let body = at + 1;
        let mut next = body;
        while next < lines.len() && stars(lines[next]).is_none() {
            next += 1;
        }
        let section = &lines[body..next];
        found.push(Section {
            heading: heading(text, keywords),
            meta: metadata(section),
            lines: section.to_vec(),
        });
        at = next;
    }
    found
}

/// Every TODO entry in one file's text, in document order. `file` is the file's bound
/// name, which an entry with no `:ID:` hashes into its id: stable while its title and
/// file stay, and independent of its state, so a state change keeps the id.
pub fn todo_entries(org: &str, file: &str) -> Vec<TodoEntry> {
    let keywords = Keywords::of(org);
    let mut seen: HashMap<String, u32> = HashMap::new();
    let mut entries = Vec::new();
    for section in sections(org, &keywords) {
        let Some(state) = section.heading.state else {
            continue;
        };
        let title = section.heading.title;
        let id = section.meta.id.clone().unwrap_or_else(|| {
            let repeat = seen.entry(title.to_string()).or_insert(0);
            let key = match *repeat {
                0 => format!("{file}|{title}"),
                n => format!("{file}|{title}|{n}"),
            };
            *repeat += 1;
            format!("org-{:016x}", fnv1a(&key))
        });
        entries.push(TodoEntry {
            id,
            state: state.to_string(),
            title: title.to_string(),
            tags: section.heading.tags.iter().map(|t| t.to_string()).collect(),
            closed: section.meta.closed.clone(),
            logbook: section.meta.logbook.clone(),
            body: body_text(&section.lines[section.meta.body_start..]),
            file: file.to_string(),
        });
    }
    entries
}

/// The lines after the metadata, with blank edges removed.
fn body_text(lines: &[&str]) -> String {
    let blank = |l: &&str| l.trim().is_empty();
    let start = lines.iter().position(|l| !blank(l)).unwrap_or(lines.len());
    let end = lines
        .iter()
        .rposition(|l| !blank(l))
        .map_or(start, |e| e + 1);
    lines[start..end].join("\n")
}

/// Every id a file holds: entries' and plain headlines' `:ID:`, and derived ones.
fn ids_in(org: &str, file: &str) -> HashSet<String> {
    let keywords = Keywords::of(org);
    let mut ids: HashSet<String> = sections(org, &keywords)
        .into_iter()
        .filter_map(|s| s.meta.id)
        .collect();
    ids.extend(todo_entries(org, file).into_iter().map(|e| e.id));
    ids
}

// ---- the renderer -----------------------------------------------------------------

/// One entry as org writes it: the headline with its tags right-aligned, the property
/// drawer with org's column for values, and the body after a blank line, which is what
/// keeps a body from being read as metadata.
fn render_entry(entry: &TodoEntry, level: usize) -> String {
    let mut head = format!("{} {} {}", "*".repeat(level), entry.state, entry.title);
    if !entry.tags.is_empty() {
        let tags = format!(":{}:", entry.tags.join(":"));
        let used = head.chars().count() + tags.chars().count();
        let pad = TAGS_COLUMN.saturating_sub(used).max(1);
        head.push_str(&" ".repeat(pad));
        head.push_str(&tags);
    }
    let mut text = format!("{head}\n:PROPERTIES:\n{:<10} {}\n:END:\n", ":ID:", entry.id);
    if !entry.body.is_empty() {
        text.push('\n');
        text.push_str(&entry.body);
        text.push('\n');
    }
    text
}

/// `org` with `entry` inserted, byte for byte otherwise: at the end of the file, or at
/// the end of the subtree of the top-level heading titled `under`.
fn insert_entry(
    org: &str,
    entry: &TodoEntry,
    under: Option<&str>,
    keywords: &Keywords,
) -> Result<String> {
    let (at, level) = match under {
        Some(title) => (subtree_end(org, title, keywords, &entry.file)?, 2),
        None => (org.len(), 1),
    };
    let crlf = org.contains("\r\n");
    let rendered = render_entry(entry, level);
    let rendered = if crlf {
        rendered.replace('\n', "\r\n")
    } else {
        rendered
    };
    let (before, after) = org.split_at(at);
    let break_first = !before.is_empty() && !before.ends_with('\n');
    let line_break = if break_first {
        if crlf {
            "\r\n"
        } else {
            "\n"
        }
    } else {
        ""
    };
    Ok(format!("{before}{line_break}{rendered}{after}"))
}

/// The byte offset just after the last non-blank line of a top-level heading's subtree.
fn subtree_end(org: &str, title: &str, keywords: &Keywords, file: &str) -> Result<usize> {
    let mut offset = 0;
    let mut matched = 0;
    let mut end = None;
    let mut inside = false;
    for line in org.split_inclusive('\n') {
        let text = line.trim_end_matches(['\r', '\n']);
        if let Some((1, head)) = stars(text) {
            inside = heading(head, keywords).title == title;
            matched += usize::from(inside);
        }
        offset += line.len();
        if inside && !text.trim().is_empty() {
            end = Some(offset);
        }
    }
    match (matched, end) {
        (1, Some(end)) => Ok(end),
        (0, _) => Err(Error::Conflict(format!(
            "urn:org:todo:file:{file}: no top-level heading is titled \"{title}\"; \
             nothing was written"
        ))),
        _ => Err(Error::Conflict(format!(
            "urn:org:todo:file:{file}: {matched} top-level headings are titled \"{title}\", \
             so `under` names none of them; nothing was written"
        ))),
    }
}

// ---- the faces --------------------------------------------------------------------

/// The door an id is served at: the id itself, percent-encoding what an IRI cannot
/// carry, so a `:ID:` written in Emacs always names a resource.
fn id_segment(id: &str) -> String {
    let mut out = String::new();
    for c in id.chars() {
        if c.is_ascii_control()
            || matches!(
                c,
                ' ' | '<' | '>' | '"' | '{' | '}' | '|' | '\\' | '^' | '`' | '#' | '?' | '%'
            )
        {
            out.push_str(&format!("%{:02X}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

fn tag_list(tags: &[String]) -> String {
    if tags.is_empty() {
        "(none)".to_string()
    } else {
        tags.join(", ")
    }
}

/// The list face: a heading, then one line per entry.
fn format_list(label: &str, entries: &[TodoEntry]) -> String {
    let mut out = format!("org todo — {label}\n\n");
    if entries.is_empty() {
        out.push_str("  (no entries)\n");
    }
    for e in entries {
        out.push_str(&format!(
            "  {}  {}  :ID: {}  tags: {}\n",
            e.state,
            e.title,
            e.id,
            tag_list(&e.tags)
        ));
    }
    out
}

/// The by-id face: every field on its own labeled line, the body last and verbatim.
fn format_entry(e: &TodoEntry) -> String {
    let mut out = format!(
        "org todo — {id}\n\nstate: {state}\ntitle: {title}\ntags: {tags}\nfile: {file}\n\
         closed: {closed}\n",
        id = e.id,
        state = e.state,
        title = e.title,
        tags = tag_list(&e.tags),
        file = e.file,
        closed = e.closed.as_deref().unwrap_or("(none)"),
    );
    if e.logbook.is_empty() {
        out.push_str("logbook: (none)\n");
    } else {
        out.push_str("logbook:\n");
        for item in &e.logbook {
            out.push_str(&format!("  {item}\n"));
        }
    }
    if e.body.is_empty() {
        out.push_str("body: (none)\n");
    } else {
        out.push_str(&format!("body:\n{}\n", e.body));
    }
    out
}

/// A `CLOSED:` stamp's inside as a floating ISO 8601 time, so the face stays a function
/// of the file and not of the host's time zone. `None` when it is not a stamp.
fn closed_time(closed: &str) -> Option<String> {
    let mut parts = closed.split_whitespace();
    let date = parts.next()?;
    date.parse::<chrono::NaiveDate>().ok()?;
    match parts.find(|p| p.contains(':')) {
        Some(time) => {
            let time = chrono::NaiveTime::parse_from_str(time, "%H:%M").ok()?;
            Some(format!("{date}T{}", time.format("%H:%M:%S")))
        }
        None => Some(date.to_string()),
    }
}

/// A Turtle string literal holding `text` exactly, line breaks included.
fn ttl_text(text: &str) -> String {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    format!("\"{escaped}\"")
}

/// The skolemized Turtle face. Terms are `ical:`'s, the namespace the vocabulary reuses
/// for calendar data, and `ik:calendar`'s provenance. No term holds a logbook note, so
/// the notes are on the text face only. Two entries claiming one id would be one
/// self-contradicting subject here, so the first keeps it.
fn format_turtle(entries: &[TodoEntry]) -> String {
    let mut ttl = String::from(
        "@prefix ical: <http://www.w3.org/2002/12/cal/ical#> .\n\
         @prefix ik: <https://ikigai-rs.dev/ns#> .\n",
    );
    let mut seen = HashSet::new();
    for e in entries.iter().filter(|e| seen.insert(e.id.as_str())) {
        let mut props = vec![
            "a ical:Vtodo".to_string(),
            format!("ical:uid {}", ttl_text(&e.id)),
            format!("ical:summary {}", ttl_text(&e.title)),
            format!("ical:status {}", ttl_text(&e.state)),
            format!("ik:calendar {}", ttl_text(&e.file)),
        ];
        for tag in &e.tags {
            props.push(format!("ical:categories {}", ttl_text(tag)));
        }
        if let Some(closed) = e.closed.as_deref().and_then(closed_time) {
            props.push(format!("ical:completed {}", ttl_text(&closed)));
        }
        if !e.body.is_empty() {
            props.push(format!("ical:description {}", ttl_text(&e.body)));
        }
        ttl.push_str(&format!(
            "\n<urn:org:todo:id:{}> {} .\n",
            id_segment(&e.id),
            props.join(" ;\n    ")
        ));
    }
    ttl
}

// ---- the endpoints ----------------------------------------------------------------

/// Parse a bound file's IRI, or refuse naming it.
fn file_iri(file: &TodoFile) -> Result<Iri> {
    Iri::parse(file.iri.as_str()).map_err(|e| {
        Error::Endpoint(format!(
            "urn:org:todo: the file named {} has a bad IRI {}: {e}",
            file.name, file.iri
        ))
    })
}

/// A representation's bytes as a bound file's text, or a refusal naming the file.
fn file_text(file: &TodoFile, bytes: Vec<u8>) -> Result<String> {
    String::from_utf8(bytes)
        .map_err(|_| Error::Endpoint(format!("urn:org:todo: {} is not UTF-8", file.iri)))
}

/// Which entries one read door serves.
#[derive(Clone, Copy)]
enum View {
    All,
    Id,
    State,
    Tag,
}

/// A read door: `urn:org:todo`, or one of its three partitions.
struct TodoEndpoint {
    id: &'static str,
    view: View,
    files: Vec<TodoFile>,
}

impl TodoEndpoint {
    async fn read_entries(&self, inv: &Invocation<'_>) -> Result<Vec<TodoEntry>> {
        if self.files.is_empty() {
            return Err(Error::Endpoint(
                "urn:org:todo: no org files bound — the host names them when it calls \
                 ikigai_org::todo_space"
                    .to_string(),
            ));
        }
        let mut entries = Vec::new();
        for file in &self.files {
            let repr = inv.source(&file_iri(file)?).await?;
            let text = file_text(file, repr.bytes)?;
            entries.extend(todo_entries(&text, &file.name));
        }
        Ok(entries)
    }

    fn binding<'a>(&self, inv: &'a Invocation<'_>, name: &str) -> Result<&'a str> {
        inv.bindings
            .get(name)
            .ok_or_else(|| Error::MissingArgument(name.to_string()))
    }
}

#[async_trait::async_trait]
impl Endpoint for TodoEndpoint {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Source {
            return Err(Error::Unsupported {
                verb: Some(inv.request.verb),
                detail: format!("{} only answers Source", self.id),
            });
        }
        let mut entries = self.read_entries(inv).await?;
        let want_turtle = inv
            .inline_str("as")
            .map(|s| s.contains("turtle"))
            .unwrap_or(false);
        let label = match self.view {
            View::All => "all entries".to_string(),
            View::Id => {
                let id = self.binding(inv, "id")?;
                entries.retain(|e| e.id == id || id_segment(&e.id) == id);
                match entries.len() {
                    0 => {
                        return Err(Error::NotFound(format!(
                            "urn:org:todo:id:{id}: no entry has this id"
                        )))
                    }
                    1 => {}
                    n => {
                        let files: Vec<&str> = entries.iter().map(|e| e.file.as_str()).collect();
                        return Err(Error::Conflict(format!(
                            "urn:org:todo:id:{id}: {n} entries claim this id, in {}; give one \
                             of them a new :ID:",
                            files.join(", ")
                        )));
                    }
                }
                String::new()
            }
            View::State => {
                let state = self.binding(inv, "state")?;
                entries.retain(|e| e.state == state);
                format!("state {state}")
            }
            View::Tag => {
                let tag = self.binding(inv, "tag")?;
                entries.retain(|e| e.tags.iter().any(|t| t == tag));
                format!("tag {tag}")
            }
        };
        let repr = if want_turtle {
            Representation::new(
                ReprType::new("text/turtle").with_param("charset", "utf-8"),
                format_turtle(&entries).into_bytes(),
            )
        } else {
            let text = match self.view {
                View::Id => format_entry(&entries[0]),
                _ => format_list(&label, &entries),
            };
            Representation::new(
                ReprType::new("text/plain").with_param("charset", "utf-8"),
                text.into_bytes(),
            )
        };
        // A function of the files alone: their threads and expiry came up through
        // `inv.source`, so over a live file space this is live too.
        Ok(repr.cacheable())
    }

    fn name(&self) -> &str {
        self.id
    }

    fn describe(&self) -> Description {
        let (title, summary, binding) = match self.view {
            View::All => (
                "Org TODO entries",
                "Every headline with a TODO keyword in the bound org files, one per line: \
                 state, title, :ID:, tags. The keywords are each file's own #+TODO line.",
                None,
            ),
            View::Id => (
                "Org TODO entry by id",
                "One entry by its :ID: property: state, title, tags, CLOSED time, LOGBOOK \
                 notes and body. NotFound when no entry has that id.",
                Some(("id", "the entry's :ID:, captured from the IRI")),
            ),
            View::State => (
                "Org TODO entries by state",
                "The entries in one state, a partition of urn:org:todo.",
                Some(("state", "the TODO keyword, captured from the IRI")),
            ),
            View::Tag => (
                "Org TODO entries by tag",
                "The entries carrying one tag of their own, a partition of urn:org:todo.",
                Some(("tag", "the tag, captured from the IRI")),
            ),
        };
        let mut action = ActionSpec::new(Verb::Source)
            .summary(summary)
            .requires(FS_READ);
        if let Some((name, about)) = binding {
            action = action.input(
                ArgSpec::new(name)
                    .summary(about)
                    .binding()
                    .class(XSD_STRING),
            );
        }
        let action = action
            .input(
                ArgSpec::new("as")
                    .summary("text/turtle for the skolemized entry graph")
                    .class(XSD_STRING)
                    .one_of(["text/plain", "text/turtle"])
                    .default_value("text/plain"),
            )
            .output("text/plain;charset=utf-8")
            .output("text/turtle;charset=utf-8");
        Description::new(self.id)
            .title(title)
            .summary(summary)
            .action(action)
    }
}

/// `urn:org:todo:file:{name}`: appends one entry to a bound file.
struct TodoSink {
    files: Vec<TodoFile>,
}

fn invalid(name: &str, detail: impl Into<String>) -> Error {
    Error::InvalidArgument {
        name: name.to_string(),
        detail: detail.into(),
    }
}

/// An optional input's value: `None` only when it was not given. One given in a form
/// this cannot read is refused, never taken for absent, or the entry would land with
/// the default state or in the wrong place.
fn optional<'a>(inv: &'a Invocation<'_>, name: &str) -> Result<Option<&'a str>> {
    if inv.request.args.contains_key(name) {
        inv.inline_str(name).map(Some)
    } else {
        Ok(None)
    }
}

/// Refuse an id that a door would have to encode, or that names nothing.
fn check_id(id: &str) -> Result<()> {
    if id.is_empty() || id_segment(id) != id || id.contains(char::is_whitespace) {
        return Err(invalid(
            "id",
            format!(
                "\"{id}\" cannot name urn:org:todo:id:{{id}}: it must be non-empty, with no \
                 white space, control characters or any of < > \" {{ }} | \\ ^ ` # ? %"
            ),
        ));
    }
    Ok(())
}

/// Refuse a title that would not read back as itself.
fn check_title(title: &str, state: &str, keywords: &Keywords) -> Result<()> {
    if title.trim().is_empty() || title != title.trim() || title.contains(['\n', '\r']) {
        return Err(invalid(
            "title",
            "must be one line of text with no white space at its edges",
        ));
    }
    let headline = format!("{state} {title}");
    let read = heading(&headline, keywords);
    if read.title != title || !read.tags.is_empty() {
        return Err(invalid(
            "title",
            format!("\"{title}\" ends in what org reads as tags; pass them in `tags`"),
        ));
    }
    Ok(())
}

/// The body as stored: unix line breaks, no blank edges, and nothing that would change
/// how the rest of the file reads. A headline would end this entry and start another; a
/// `#+TODO:` outside a block would change every entry's keywords.
fn checked_body(body: &str) -> Result<String> {
    let body = body.replace("\r\n", "\n");
    let lines: Vec<&str> = body.lines().collect();
    let body = body_text(&lines);
    let mut block = false;
    for line in body.lines() {
        if stars(line).is_some() {
            return Err(invalid(
                "content",
                format!(
                    "the line \"{line}\" would start a new org entry; indent it, or write it \
                     as org does inside a src block, with a leading comma"
                ),
            ));
        }
        let upper = line.trim_start().to_ascii_uppercase();
        if upper.starts_with("#+BEGIN_") {
            block = true;
        } else if upper.starts_with("#+END_") {
            block = false;
        } else if !block && keyword_line(line).is_some() {
            return Err(invalid(
                "content",
                format!(
                    "the line \"{line}\" would change the TODO keywords of the whole file; put \
                     it inside a src block or remove it"
                ),
            ));
        }
    }
    Ok(body)
}

impl TodoSink {
    /// Refuse unless the file's Sink declares `if-match`, by asking the kernel's own
    /// pre-flight. A file space that does not declare it may ignore it, and an ignored
    /// token is a blind overwrite of whatever was saved since the read.
    async fn require_conditional_write(&self, inv: &Invocation<'_>, file: &Iri) -> Result<()> {
        let token = format!("b3:{}", "0".repeat(64));
        let validate = Iri::parse("urn:kernel:validate").expect("a valid IRI");
        let request = Request::new(Verb::Source, validate)
            .with_arg(
                "endpoint",
                ArgRef::Inline(file.as_str().as_bytes().to_vec()),
            )
            .with_arg("verb", ArgRef::Inline(b"sink".to_vec()))
            .with_arg(
                "args",
                ArgRef::Inline(format!("content=x&if-match={token}").into_bytes()),
            );
        let report = inv.issue(request).await?;
        if String::from_utf8_lossy(&report.bytes).contains("sh:conforms true") {
            return Ok(());
        }
        Err(Error::Unsupported {
            verb: Some(Verb::Sink),
            detail: format!(
                "{file}: its file space does not declare a conditional write (`if-match`), so \
                 appending could overwrite an edit; nothing was written. ikigai-fs serves one \
                 from 0.4.1"
            ),
        })
    }
}

#[async_trait::async_trait]
impl Endpoint for TodoSink {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb != Verb::Sink {
            return Err(Error::Unsupported {
                verb: Some(inv.request.verb),
                detail: "org-todo-file only answers Sink; read urn:org:todo".to_string(),
            });
        }
        let name = inv
            .bindings
            .get("name")
            .ok_or_else(|| Error::MissingArgument("name".to_string()))?;
        let Some(file) = self.files.iter().find(|f| f.name == name) else {
            let bound: Vec<&str> = self.files.iter().map(|f| f.name.as_str()).collect();
            return Err(Error::NotFound(format!(
                "urn:org:todo:file:{name}: no bound org file has this name; bound: {}",
                bound.join(", ")
            )));
        };
        let id = inv.inline_str("id")?.to_string();
        check_id(&id)?;
        let title = inv.inline_str("title")?.to_string();
        let tags: Vec<String> = optional(inv, "tags")?
            .unwrap_or_default()
            .split([',', ' ', '\t', '\n'])
            .filter(|t| !t.is_empty())
            .map(str::to_string)
            .collect();
        if let Some(bad) = tags.iter().find(|t| !is_tag(t)) {
            return Err(invalid(
                "tags",
                format!("\"{bad}\" is not an org tag: letters, digits, _ @ # % only"),
            ));
        }
        let body = if inv.request.args.contains_key("content") {
            let bytes = inv.collect(inv.content_body("content").await?).await?;
            let text = std::str::from_utf8(&bytes)
                .map_err(|_| invalid("content", "the entry body must be UTF-8 text"))?;
            checked_body(text)?
        } else {
            String::new()
        };

        let iri = file_iri(file)?;
        self.require_conditional_write(inv, &iri).await?;

        // The version the write is against: its content id goes out as `if-match`.
        let read = inv.source(&iri).await?;
        let token = read.content_id();
        let org = file_text(file, read.bytes)?;

        let keywords = Keywords::of(&org);
        let state = match optional(inv, "state")? {
            Some(state) => state.to_string(),
            None => keywords
                .open
                .first()
                .cloned()
                .unwrap_or_else(|| "TODO".to_string()),
        };
        if !keywords.contains(&state) {
            return Err(invalid(
                "state",
                format!(
                    "\"{state}\" is not a keyword of {name}, whose keywords are {}",
                    keywords.listed()
                ),
            ));
        }
        check_title(&title, &state, &keywords)?;

        // An id names one resource across every bound file.
        let mut held = ids_in(&org, &file.name)
            .contains(&id)
            .then(|| file.name.clone());
        for other in self.files.iter().filter(|f| f.name != file.name) {
            if held.is_some() {
                break;
            }
            let text = file_text(other, inv.source(&file_iri(other)?).await?.bytes)?;
            if ids_in(&text, &other.name).contains(&id) {
                held = Some(other.name.clone());
            }
        }
        if let Some(holder) = held {
            return Err(Error::Conflict(format!(
                "urn:org:todo:id:{id} already exists, in {holder}; nothing was written"
            )));
        }

        let entry = TodoEntry {
            id: id.clone(),
            state,
            title,
            tags,
            closed: None,
            logbook: Vec::new(),
            body,
            file: file.name.clone(),
        };
        let under = optional(inv, "under")?;
        let text = insert_entry(&org, &entry, under, &keywords)?;
        inv.issue(
            Request::new(Verb::Sink, iri)
                .with_arg("content", ArgRef::Inline(text.into_bytes()))
                .with_arg("if-match", ArgRef::Inline(token.to_string().into_bytes())),
        )
        .await?;
        Ok(Representation::new(
            ReprType::new("text/plain").with_param("charset", "utf-8"),
            format!("appended urn:org:todo:id:{id} to {name}\n").into_bytes(),
        ))
    }

    fn name(&self) -> &str {
        "org-todo-file"
    }

    fn describe(&self) -> Description {
        let summary = "Append one entry to the bound org file the host names {name}, with an \
                       if-match write against the version read: Conflict if the file changed \
                       in between or the id exists. Nothing else in the file changes.";
        let text = |name: &str, about: &str| {
            ArgSpec::new(name.to_string())
                .summary(about.to_string())
                .class(XSD_STRING)
        };
        Description::new("org-todo-file")
            .title("Append an org TODO entry")
            .summary(summary)
            .action(
                ActionSpec::new(Verb::Sink)
                    .summary(summary)
                    // The write reads the file first, under the caller's own capability,
                    // to find the version it writes against. So it enforces both.
                    .requires(FS_READ)
                    .requires(FS_WRITE)
                    .input(text("name", "the bound file's name, captured from the IRI").binding())
                    .input(text("id", "the entry's stable :ID:"))
                    .input(text("title", "the headline text, one line"))
                    .input(
                        text("state", "a keyword of the file; default its first open one")
                            .optional(),
                    )
                    .input(
                        text("tags", "the entry's tags")
                            .list(ListSeparator::CommaOrWhitespace)
                            .optional(),
                    )
                    .input(
                        text(
                            "under",
                            "a top-level heading to file it beneath, as a child",
                        )
                        .optional(),
                    )
                    .input(text("content", "the entry's body, org text").optional())
                    .output("text/plain;charset=utf-8"),
            )
    }
}

/// Mount the TODO doors over the files the host binds: `urn:org:todo`, its partitions
/// `urn:org:todo:id:{id}`, `urn:org:todo:state:{state}` and `urn:org:todo:tag:{tag}`,
/// and the Sink `urn:org:todo:file:{name}`.
pub fn todo_space(files: Vec<TodoFile>) -> EndpointSpace {
    let door = |id, view, files: &Vec<TodoFile>| TodoEndpoint {
        id,
        view,
        files: files.clone(),
    };
    let template = |t: &str| UriTemplate::parse(t).expect("valid template");
    EndpointSpace::new()
        .bind(
            Exact::new("urn:org:todo"),
            door("org-todo", View::All, &files),
        )
        .bind(
            template("urn:org:todo:id:{id}"),
            door("org-todo-id", View::Id, &files),
        )
        .bind(
            template("urn:org:todo:state:{state}"),
            door("org-todo-state", View::State, &files),
        )
        .bind(
            template("urn:org:todo:tag:{tag}"),
            door("org-todo-tag", View::Tag, &files),
        )
        .bind(template("urn:org:todo:file:{name}"), TodoSink { files })
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUEUE: &str = "\
#+TITLE: Queue
#+TODO: TODO(t) NEXT(n!) WAIT(w@/!) | DONE(d!) CANCELED(c@)

* Inbox
** WAIT Read the contract                                             :legal:work:
CLOSED: [2026-11-04 Wed 10:00] SCHEDULED: <2026-11-05 Thu>
:PROPERTIES:
:ID:       contract-1
:OWNER:    hub
:END:
:LOGBOOK:
- State \"WAIT\"       from \"NEXT\"       [2026-11-04 Wed 09:00] \\\\
  waiting on counsel
CLOCK: [2026-11-04 Wed 08:00]--[2026-11-04 Wed 08:30] =>  0:30
- Note taken on [2026-11-03 Tue 12:00]
:END:

See [[https://example.com][the draft]].
:ID: hijacked

#+BEGIN_SRC org
* not a heading? it is, unless a comma guards it
#+END_SRC
** Plain note
:PROPERTIES:
:ID:       plain-1
:END:
** DONE Without an id
  * an indented star is a list item
* NOT-A-KEYWORD Stays a heading
";

    #[test]
    fn keywords_come_from_the_file_with_marks_dropped() {
        let k = Keywords::of(QUEUE);
        assert_eq!(k.open, ["TODO", "NEXT", "WAIT"]);
        assert_eq!(k.done, ["DONE", "CANCELED"]);
    }

    #[test]
    fn keywords_default_and_the_last_one_is_done_with_no_bar() {
        let k = Keywords::of("* TODO x\n");
        assert_eq!((k.open, k.done), (vec!["TODO".into()], vec!["DONE".into()]));
        let k = Keywords::of("#+SEQ_TODO: A B C\n");
        assert_eq!(k.open, ["A", "B"]);
        assert_eq!(k.done, ["C"]);
    }

    #[test]
    fn a_keyword_line_inside_a_block_is_an_example() {
        let k = Keywords::of("#+BEGIN_SRC org\n#+TODO: X | Y\n#+END_SRC\n");
        assert_eq!(k.open, ["TODO"]);
    }

    #[test]
    fn an_entry_reads_whole() {
        let entries = todo_entries(QUEUE, "queue");
        let e = &entries[0];
        assert_eq!(e.id, "contract-1");
        assert_eq!(e.state, "WAIT");
        assert_eq!(e.title, "Read the contract");
        assert_eq!(e.tags, ["legal", "work"]);
        assert_eq!(e.closed.as_deref(), Some("2026-11-04 Wed 10:00"));
        assert_eq!(
            e.logbook,
            [
                "State \"WAIT\" from \"NEXT\" [2026-11-04 Wed 09:00] waiting on counsel",
                "Note taken on [2026-11-03 Tue 12:00]",
            ]
        );
        assert!(
            e.body.starts_with("See [[https://example.com]"),
            "{}",
            e.body
        );
        assert!(e.body.contains(":ID: hijacked"), "body text, not identity");
    }

    #[test]
    fn only_a_headline_with_a_keyword_is_an_entry() {
        let entries = todo_entries(QUEUE, "queue");
        let titles: Vec<&str> = entries.iter().map(|e| e.title.as_str()).collect();
        assert_eq!(titles, ["Read the contract", "Without an id"]);
        assert_eq!(
            entries[1].body, "  * an indented star is a list item",
            "column zero makes a headline"
        );
    }

    #[test]
    fn a_body_id_never_seizes_and_a_derived_id_survives_a_state_change() {
        let done = todo_entries(QUEUE, "queue");
        assert!(done[1].id.starts_with("org-"), "{}", done[1].id);
        let waiting = QUEUE.replace("** DONE Without", "** WAIT Without");
        assert_eq!(todo_entries(&waiting, "queue")[1].id, done[1].id);
        assert_ne!(todo_entries(QUEUE, "other")[1].id, done[1].id);
    }

    #[test]
    fn two_entries_with_one_title_get_two_derived_ids() {
        let entries = todo_entries("* TODO Same\n* TODO Same\n", "q");
        assert_ne!(entries[0].id, entries[1].id);
    }

    #[test]
    fn an_unterminated_drawer_stays_body_text() {
        let entries = todo_entries("* TODO x\n:LOGBOOK:\nkeep me\n", "q");
        assert_eq!(entries[0].body, ":LOGBOOK:\nkeep me");
    }

    #[test]
    fn a_headline_of_only_a_keyword_or_only_tags_parses() {
        let k = Keywords::of("");
        let only = heading("TODO", &k);
        assert_eq!((only.state, only.title), (Some("TODO"), ""));
        let tags = heading("TODO :a:b:", &k);
        assert_eq!((tags.title, tags.tags), ("", vec!["a", "b"]));
    }

    fn entry(id: &str, body: &str, tags: &[&str]) -> TodoEntry {
        TodoEntry {
            id: id.to_string(),
            state: "TODO".to_string(),
            title: "New".to_string(),
            tags: tags.iter().map(|t| t.to_string()).collect(),
            closed: None,
            logbook: Vec::new(),
            body: body.to_string(),
            file: "q".to_string(),
        }
    }

    #[test]
    fn the_renderer_writes_what_org_writes() {
        assert_eq!(
            render_entry(&entry("a-1", "Body", &[]), 1),
            "* TODO New\n:PROPERTIES:\n:ID:       a-1\n:END:\n\nBody\n"
        );
        let tagged = render_entry(&entry("a-1", "", &["x", "y"]), 2);
        let head = tagged.lines().next().unwrap();
        assert_eq!(head.chars().count(), TAGS_COLUMN, "right-aligned: {head}");
        assert!(head.starts_with("** TODO New ") && head.ends_with(" :x:y:"));
    }

    #[test]
    fn insertion_adds_bytes_and_changes_none() {
        let k = Keywords::of("");
        let new = entry("a-1", "", &[]);
        for org in ["", "* TODO a\n", "* TODO a\n\n\n", "* TODO a", "x\r\ny\r\n"] {
            let out = insert_entry(org, &new, None, &k).unwrap();
            assert!(out.starts_with(org), "{org:?} -> {out:?}");
            assert_eq!(todo_entries(&out, "q").last().unwrap().id, "a-1", "{out:?}");
        }
        let crlf = insert_entry("x\r\n", &new, None, &k).unwrap();
        assert_eq!(crlf.matches('\n').count(), crlf.matches("\r\n").count());
    }

    #[test]
    fn under_files_at_the_end_of_that_subtree() {
        let k = Keywords::of("");
        let org = "* Inbox :q:\n** TODO old\nbody\n\n* Later\n";
        let out = insert_entry(org, &entry("a-1", "", &[]), Some("Inbox"), &k).unwrap();
        assert_eq!(
            out,
            "* Inbox :q:\n** TODO old\nbody\n** TODO New\n:PROPERTIES:\n:ID:       a-1\n:END:\n\n* Later\n"
        );
        let out = insert_entry("* Inbox", &entry("a-1", "", &[]), Some("Inbox"), &k).unwrap();
        assert!(out.starts_with("* Inbox\n** TODO New\n"), "{out}");
    }

    #[test]
    fn under_refuses_no_heading_and_two_headings() {
        let k = Keywords::of("");
        let new = entry("a-1", "", &[]);
        for org in ["** Inbox\n", "* Inbox\n* Inbox\n"] {
            let e = insert_entry(org, &new, Some("Inbox"), &k).unwrap_err();
            assert!(matches!(e, Error::Conflict(_)), "{e}");
        }
    }

    #[test]
    fn a_body_cannot_change_how_the_file_reads() {
        assert!(checked_body("* Headline").is_err());
        assert!(checked_body("#+TODO: A | B").is_err());
        let fine = "  * list\n,* guarded\n#+BEGIN_SRC org\n#+TODO: A | B\n#+END_SRC";
        assert_eq!(checked_body(fine).unwrap(), fine);
        assert_eq!(checked_body("\n\r\nx\r\ny\n\n").unwrap(), "x\ny");
    }

    #[test]
    fn a_title_or_id_that_would_not_read_back_is_refused() {
        let k = Keywords::of("");
        check_title("Plain: title", "TODO", &k).unwrap();
        assert!(check_title("ends in :tags:", "TODO", &k).is_err());
        assert!(check_title("two\nlines", "TODO", &k).is_err());
        assert!(check_title(" ", "TODO", &k).is_err());
        check_id("76F47687-B4DF@host.example:a").unwrap();
        for bad in ["", "a b", "a<b", "50%"] {
            assert!(check_id(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn an_id_org_can_hold_still_names_a_door() {
        assert_eq!(id_segment("a b%"), "a%20b%25");
        let ttl = format_turtle(&[entry("a b", "line\n\"two\"", &["x"])]);
        assert!(
            ttl.contains("<urn:org:todo:id:a%20b> a ical:Vtodo"),
            "{ttl}"
        );
        assert!(
            ttl.contains("ical:description \"line\\n\\\"two\\\"\""),
            "{ttl}"
        );
    }

    #[test]
    fn a_closed_stamp_is_floating_time() {
        assert_eq!(
            closed_time("2026-11-04 Wed 10:00").as_deref(),
            Some("2026-11-04T10:00:00")
        );
        assert_eq!(closed_time("2026-11-04 Wed").as_deref(), Some("2026-11-04"));
        assert_eq!(closed_time("someday"), None);
    }

    #[test]
    fn the_text_faces_say_everything_in_words() {
        assert_eq!(
            format_list("all entries", &[]),
            "org todo — all entries\n\n  (no entries)\n"
        );
        let line = format_list("x", &[entry("a-1", "", &[])]);
        assert!(
            line.contains("  TODO  New  :ID: a-1  tags: (none)\n"),
            "{line}"
        );
        let one = format_entry(&entry("a-1", "", &["x", "y"]));
        assert_eq!(
            one,
            "org todo — a-1\n\nstate: TODO\ntitle: New\ntags: x, y\nfile: q\nclosed: (none)\n\
             logbook: (none)\nbody: (none)\n"
        );
    }
}

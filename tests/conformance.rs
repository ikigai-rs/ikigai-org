//! The module recipe as one test: `ikigai-conformance` walks the one endpoint
//! [`ikigai_org::space`] binds — `org-agenda`, at `urn:org:agenda` and
//! `urn:org:agenda:{period}` — and reports every violation at once.
//!
//! ## The fixture is a file space, because the agenda reads through the kernel
//!
//! `urn:org:agenda:*` never opens a file: it `inv.source`s the org-file IRIs the
//! host hands it (the real host binds `urn:orgfile:{path}` to an `ikigai-fs`
//! `FileEndpoint` jailed to the org directory). So the kernel under test is the
//! agenda space over an in-memory [`OrgFile`] at the same grammar, holding one
//! org file with every property the faces render (an `:ID:`, a location, a join
//! link, alerts, a repeater, an all-day stamp), counting its reads. It is walked
//! as a module endpoint too (the suite walks everything bound), so it conforms
//! itself. Two walks, because the host decides how the files are served:
//!
//! - **cacheable** ([`conforms`]): the file is `.cacheable()` under the thread
//!   `urn:orgfile:calendar.org`, as `ikigai_fs::cacheable_space` serves one. The
//!   agenda inherits that thread through the sub-resolution and is declared
//!   `cacheable`, so the suite holds it to a real cache hit with a non-empty
//!   thread set — the golden-thread promise the crate docs make.
//! - **live** ([`a_live_file_space_leaves_the_agenda_live`]): the file is served
//!   uncacheable, which is what the host mounts today. The effective expiry is
//!   the least cacheable part's, so the agenda is live too — correct, and the
//!   test shows the exact finding `cacheable` would cost over this space.
//!
//! The kernel carries a [`FixedClock`]: a relative period (`today`, `week`) reads
//! the date from the kernel's clock, so the walk over `urn:org:agenda` (bare =
//! week) is deterministic and its `Expiry::At` deadline is honored.
//!
//! ## What the suite cannot hold and this file pins by hand
//!
//! - **Edit + cut recomputes; no cut serves stale**
//!   ([`an_edit_needs_a_cut_and_a_cut_recomputes`]), with the read counter —
//!   the suite's second resolution IS the cache hit, so it never sees a
//!   recomputation (its PENDING #64).
//! - **A relative period is cached until local midnight, an absolute one
//!   outright, and a clockless kernel caches nothing relative**
//!   ([`a_relative_period_expires_at_local_midnight`],
//!   [`a_clockless_kernel_serves_relative_periods_live`]): the clock is core's
//!   thread for time, and the suite has no declaration for "cacheable until".
//! - **Declared outputs are the media types served, both directions, driven
//!   from `as`'s `one_of`** ([`declared_outputs_are_the_media_types_served`]):
//!   the suite compares only RDF faces, and only once they are declared — the
//!   Turtle face went unprobed for six releases because `as=text/turtle` was
//!   served without being an output (PENDING #11/#31/#79).
//! - **The capability gate is the HOST's file space's, not this module's**
//!   ([`the_file_gate_is_the_hosts_and_passes_through_typed`]): `org-agenda`
//!   declares no `requires` because it cannot know what the host's file space
//!   enforces (`urn:cap:fs:read:*` for `ikigai-fs`); the gate is reached through
//!   `inv.source`, and the typed `Denied` passes through unwrapped. ENFORCED
//!   cannot see a gate in a sub-resolution (PENDING #21/#29/#32); the test shows
//!   the over-offer finding a gated fixture produces, which is a true statement
//!   about the composed host that this crate alone cannot fix.
//! - **The fixture ids are the description ids**
//!   ([`the_fixture_ids_are_the_description_ids`]): a `Fixture` that matches no
//!   description is silently inert (PENDING #57).
//!
//! No opt-outs, no module namespace (the Turtle face uses `ical:` and three
//! `ik:` terms `ikigai-vocab` defines), NAMES runs (`org-agenda` is kebab-case).

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{Duration, Local, NaiveDateTime, NaiveTime, TimeZone, Utc};
use ikigai_conformance::{rdf, Check, Fixture, Report, Suite};
use ikigai_core::{
    ArgRef, ArgSpec, Capability, Description, Endpoint, EndpointSpace, Error, Expiry, Fallback,
    FixedClock, Invocation, Iri, Kernel, ReprType, Representation, Request, Result, Time,
    UriTemplate, Verb,
};

/// The agenda's description id, at both of its patterns.
const AGENDA: &str = "org-agenda";
/// The fixture file space's description id.
const ORGFILE: &str = "orgfile";

/// The one org file the fixture serves, and the IRI the agenda reads it at —
/// which is also the golden thread a cacheable read declares.
const FILE: &str = "calendar.org";
const FILE_IRI: &str = "urn:orgfile:calendar.org";

/// An absolute period covering the fixture's events: a function of the file
/// alone, so the walk over the template entry does not depend on the clock.
const ABSOLUTE: &str = "2026-07";

const AGENDA_IRI: &str = "urn:org:agenda";
const ABSOLUTE_IRI: &str = "urn:org:agenda:2026-07";
const TODAY_IRI: &str = "urn:org:agenda:today";

/// What the real host's file space enforces (`ikigai-fs`'s read scope), for the
/// gated variant of the fixture.
const FS_READ: &str = "urn:cap:fs:read:*";

const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const ICAL: &str = "http://www.w3.org/2002/12/cal/ical#";
const IK: &str = "https://ikigai-rs.dev/ns#";

/// The fixture org file: every property the faces render, in both drawer
/// positions Emacs produces.
const ORG: &str = "\
#+TITLE: Calendar

* Dinner with the Hendersons
  :PROPERTIES:
  :ID:       dinner-2026-07-11
  :LOCATION: Chez Panisse
  :END:
  :ALERT: 1h 1d
  <2026-07-11 Sat 19:00-21:00>

* Trash & recycling out
  <2026-07-03 Fri 07:00 +1w>

* Standup
  SCHEDULED: <2026-07-15 Wed 09:00-09:15>
  :PROPERTIES:
  :URL: https://us06web.zoom.us/j/1234
  :END:

* Independence Day
  <2026-07-04 Sat>
";

/// The entry an edit appends, to prove recomputation by content.
const EDIT: &str = "\n* Board game night\n  <2026-07-20 Mon 19:00-22:00>\n";

/// The kernel's instant for every clocked test: noon UTC on a Wednesday inside
/// the fixture's month, so the local date is 2026-07-15 in every zone within
/// twelve hours of UTC and `week` lands on the fixture's events.
fn noon() -> u64 {
    u64::try_from(
        Utc.with_ymd_and_hms(2026, 7, 15, 12, 0, 0)
            .single()
            .expect("a real instant")
            .timestamp_millis(),
    )
    .expect("after the epoch")
}

/// An in-memory org file at `urn:orgfile:{path}`: what the host's `ikigai-fs`
/// mount serves, without a filesystem. `cacheable` selects between the two ways
/// a host can mount it; `gate` adds the read scope the real mount declares, so
/// the kernel refuses an ungranted reader before this endpoint runs.
struct OrgFile {
    text: Mutex<String>,
    reads: AtomicUsize,
    cacheable: bool,
    gate: Option<&'static str>,
}

impl OrgFile {
    fn new(cacheable: bool, gate: Option<&'static str>) -> Arc<OrgFile> {
        Arc::new(OrgFile {
            text: Mutex::new(ORG.to_string()),
            reads: AtomicUsize::new(0),
            cacheable,
            gate,
        })
    }

    fn reads(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    fn append(&self, text: &str) {
        self.text.lock().expect("text lock").push_str(text);
    }
}

#[async_trait::async_trait]
impl Endpoint for OrgFile {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        let path = inv.bindings.get("path").unwrap_or_default();
        if path != FILE {
            return Err(Error::NotFound(format!("urn:orgfile:{path}")));
        }
        self.reads.fetch_add(1, Ordering::SeqCst);
        let text = self.text.lock().expect("text lock").clone();
        let repr = Representation::new(ReprType::new("text/plain"), text.into_bytes());
        Ok(if self.cacheable {
            repr.cacheable().depends_on(inv.request.target.as_str())
        } else {
            repr
        })
    }

    fn name(&self) -> &str {
        ORGFILE
    }

    fn describe(&self) -> Description {
        let description = Description::new(ORGFILE)
            .summary("an org file, in memory (the conformance fixture)")
            .verb(Verb::Source)
            .input(ArgSpec::new("path").binding().class(XSD_STRING))
            .output("text/plain");
        match self.gate {
            Some(scope) => description.requires(scope),
            None => description,
        }
    }
}

/// The kernel under test: the agenda space over one fixture file, with or
/// without a clock.
struct Agenda {
    kernel: Kernel,
    file: Arc<OrgFile>,
}

impl Agenda {
    fn new(file: Arc<OrgFile>, clock: Option<u64>) -> Agenda {
        let files = EndpointSpace::new().bind_arc(
            UriTemplate::parse("urn:orgfile:{path}").expect("a valid template"),
            file.clone(),
        );
        let space = Fallback::new(vec![
            Arc::new(files),
            Arc::new(ikigai_org::space(vec![FILE_IRI.to_string()])),
        ]);
        let mut kernel = Kernel::new(Arc::new(space));
        if let Some(millis) = clock {
            kernel = kernel.with_clock(Arc::new(FixedClock::at(millis)));
        }
        Agenda { kernel, file }
    }

    /// The host's golden-thread-ready shape: a cacheable file space and a clock.
    fn cacheable() -> Agenda {
        Agenda::new(OrgFile::new(true, None), Some(noon()))
    }

    fn issue(&self, request: Request, capability: &Capability) -> Result<Representation> {
        futures::executor::block_on(self.kernel.issue(request, capability))
    }

    fn resolve(&self, request: Request) -> Representation {
        self.issue(request, &Capability::root())
            .unwrap_or_else(|e| panic!("resolution failed: {e}"))
    }

    fn text(&self, request: Request) -> String {
        String::from_utf8(self.resolve(request).bytes).expect("UTF-8")
    }

    fn is_cached(&self, request: &Request) -> bool {
        self.kernel.is_cached(request, &Capability::root())
    }
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    let mut request = Request::new(verb, Iri::parse(iri).expect("a valid IRI"));
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    request
}

fn source(iri: &str) -> Request {
    request(Verb::Source, iri, &[])
}

/// The suite, configured for this module: the fixture file bound by name (the
/// derived `x` names no file) and an absolute period for the template entry.
fn suite() -> Suite {
    Suite::new()
        .fixture(Fixture::new(ORGFILE, Verb::Source).binding("path", FILE))
        .fixture(Fixture::new(AGENDA, Verb::Source).binding("period", ABSOLUTE))
}

/// The walk saw the agenda and the fixture, three actions (the agenda's two
/// entries, the file's one), and skipped nothing. A second endpoint bound by
/// `space()` without a line here would be held to a weaker standard.
fn assert_shape(report: &Report) {
    assert_eq!(
        report.endpoints, 2,
        "org-agenda + the fixture file: {report}"
    );
    assert_eq!(
        report.actions, 3,
        "Source at urn:org:agenda, at urn:org:agenda:{{period}}, and on the file: {report}"
    );
    assert_eq!(
        report.checks.skipped().count(),
        0,
        "every check runs: {report}"
    );
}

/// The findings as (endpoint, verb, check), for pinning exact lists.
fn findings(report: &Report) -> Vec<(&str, Option<Verb>, Check)> {
    report
        .findings
        .iter()
        .map(|f| (f.endpoint.as_str(), f.verb, f.check))
        .collect()
}

#[test]
fn conforms() {
    let agenda = Agenda::cacheable();
    let report = suite()
        .cacheable(AGENDA)
        .cacheable(ORGFILE)
        .run_blocking(&agenda.kernel);
    // Printed even when clean (`--nocapture`): the report is the record.
    eprintln!("[cacheable file space]\n{report}");
    assert!(report.is_clean(), "{report}");
    assert_shape(&report);

    // The footprint: the file was read under root once (the cache probe's first
    // resolution; its second was the hit) and once under ENFORCED's no-grants
    // capability (a different cache key). Both agenda entries — each resolved
    // twice under root and once under no grants — hit the cached file every
    // time. Six agenda resolutions, zero file reads: that is the golden thread
    // working, and this number is what changes if it stops.
    assert_eq!(
        agenda.file.reads(),
        2,
        "one read under root, one under no grants; the agenda never re-read the file"
    );
}

/// The host's current mount: `urn:orgfile:{path}` on an `ikigai-fs`
/// `FileEndpoint` that is NOT `.cacheable()`. The walk is clean without a
/// declaration — the agenda's effective expiry is the file's, `Always` — and
/// declaring `cacheable` over this space is exactly the red line the suite
/// exists for (README: "that silent downgrade ... declared, it is a red test").
#[test]
fn a_live_file_space_leaves_the_agenda_live() {
    let agenda = Agenda::new(OrgFile::new(false, None), Some(noon()));
    let report = suite().run_blocking(&agenda.kernel);
    eprintln!("[live file space]\n{report}");
    assert!(report.is_clean(), "{report}");
    assert_shape(&report);

    let before = agenda.file.reads();
    let first = agenda.resolve(source(ABSOLUTE_IRI));
    let second = agenda.resolve(source(ABSOLUTE_IRI));
    assert_eq!(first.expiry, Expiry::Always, "live file, live agenda");
    assert_eq!(first.bytes, second.bytes);
    assert!(!agenda.is_cached(&source(ABSOLUTE_IRI)));
    assert_eq!(
        agenda.file.reads(),
        before + 2,
        "every agenda read re-reads a live file"
    );

    // Declared cacheable over a live file space: one finding per agenda entry,
    // naming the declaration, and nothing else.
    let report = suite().cacheable(AGENDA).run_blocking(&agenda.kernel);
    eprintln!("[live file space, org-agenda declared cacheable]\n{report}");
    assert_eq!(
        findings(&report),
        [
            (AGENDA, Some(Verb::Source), Check::Cacheable),
            (AGENDA, Some(Verb::Source), Check::Cacheable),
        ],
        "{report}"
    );
    assert!(
        report
            .findings
            .iter()
            .all(|f| f.detail.contains("declared cacheable")),
        "{report}"
    );
}

/// The cache contract, with the read counter the suite lacks: an absolute
/// period is served from the cache under exactly the file's thread; editing the
/// file without cutting serves the OLD agenda (stale by design — a host that
/// caches its files must cut on change, as `ikigai-fs`'s Sink and the
/// filesystem watcher do); cutting recomputes from the edited file.
#[test]
fn an_edit_needs_a_cut_and_a_cut_recomputes() {
    let agenda = Agenda::cacheable();

    let first = agenda.resolve(source(ABSOLUTE_IRI));
    let text = String::from_utf8(first.bytes.clone()).expect("UTF-8");
    assert!(text.contains("Dinner with the Hendersons"), "{text}");
    assert!(!text.contains("Board game night"), "{text}");
    assert_eq!(
        first.expiry,
        Expiry::Never,
        "absolute: a function of the file"
    );
    let threads: Vec<String> = first.threads().iter().map(|t| t.to_string()).collect();
    assert_eq!(
        threads,
        [FILE_IRI],
        "cached under exactly the file's thread"
    );
    assert!(agenda.is_cached(&source(ABSOLUTE_IRI)));
    assert_eq!(agenda.file.reads(), 1);

    // Edit without a cut: the cache answers, the file is not read, the new
    // event is not there.
    agenda.file.append(EDIT);
    let stale = agenda.resolve(source(ABSOLUTE_IRI));
    assert_eq!(
        stale.bytes, first.bytes,
        "no cut: the cached agenda is served"
    );
    assert_eq!(agenda.file.reads(), 1, "no cut: the file was not re-read");

    // Cut the file's thread (what a Sink through the kernel or a watcher does):
    // the next read recomputes from the edited file.
    agenda.kernel.cut(FILE_IRI);
    assert!(
        !agenda.is_cached(&source(ABSOLUTE_IRI)),
        "the cut evicted it"
    );
    let fresh = agenda.text(source(ABSOLUTE_IRI));
    assert!(fresh.contains("Board game night"), "{fresh}");
    assert_eq!(agenda.file.reads(), 2, "the cut forced one recomputation");
    assert!(
        agenda.is_cached(&source(ABSOLUTE_IRI)),
        "and it is cached again"
    );

    // The Turtle face is a separate request identity, computed on its own under
    // the same thread, and sees the edit too — from the file the recomputation
    // just re-cached, not from a third read.
    let ttl = agenda.text(request(
        Verb::Source,
        ABSOLUTE_IRI,
        &[("as", "text/turtle")],
    ));
    assert!(ttl.contains("Board game night"), "{ttl}");
    assert_eq!(
        agenda.file.reads(),
        2,
        "the file was cached again by the recomputation"
    );
}

/// A relative period is a function of today as well as the file, and today is
/// the KERNEL's clock: under a fixed clock the window is that clock's local
/// date, and the result is cacheable until the next local midnight — the
/// instant every relative window computed from that date stops being true.
#[test]
fn a_relative_period_expires_at_local_midnight() {
    let agenda = Agenda::cacheable();
    let today = Local
        .timestamp_millis_opt(i64::try_from(noon()).expect("fits"))
        .single()
        .expect("a real instant")
        .date_naive();
    let midnight = Local
        .from_local_datetime(&NaiveDateTime::new(
            today + Duration::days(1),
            NaiveTime::from_hms_opt(0, 0, 0).expect("midnight"),
        ))
        .earliest()
        .expect("this zone has a midnight tomorrow");
    let deadline = Time::from_millis(u64::try_from(midnight.timestamp_millis()).expect("fits"));

    let text = agenda.text(source(TODAY_IRI));
    assert!(
        text.starts_with(&format!("org agenda — {today}\n")),
        "today is the kernel clock's local date: {text}"
    );
    let repr = agenda.resolve(source(TODAY_IRI));
    assert_eq!(
        repr.expiry,
        Expiry::At(deadline),
        "cacheable until the next local midnight, and no later"
    );
    assert!(
        repr.threads().iter().any(|t| t.to_string() == FILE_IRI),
        "and still under the file's thread"
    );
    assert!(
        agenda.is_cached(&source(TODAY_IRI)),
        "the clock is before the deadline"
    );

    // Bare = week: relative too, same deadline.
    let week = agenda.resolve(source(AGENDA_IRI));
    assert_eq!(week.expiry, Expiry::At(deadline));
    let text = String::from_utf8(week.bytes).expect("UTF-8");
    assert!(text.contains("Standup"), "the week of the clock: {text}");

    // A month NAME is relative (July of the clock's year); YYYY-MM is not.
    assert_eq!(
        agenda.resolve(source("urn:org:agenda:july")).expiry,
        Expiry::At(deadline)
    );
    assert_eq!(agenda.resolve(source(ABSOLUTE_IRI)).expiry, Expiry::Never);
    assert_eq!(
        agenda
            .resolve(source("urn:org:agenda:2026-07-01..2026-07-31"))
            .expiry,
        Expiry::Never
    );
}

/// Without a kernel clock the date comes from the wall clock, and a relative
/// result is not cached at all: there is no clock to expire it against, and a
/// wall-clock window cached forever would be wrong tomorrow. Absolute periods
/// need no clock and cache as before.
#[test]
fn a_clockless_kernel_serves_relative_periods_live() {
    let agenda = Agenda::new(OrgFile::new(true, None), None);

    let before = agenda.file.reads();
    let first = agenda.resolve(source(TODAY_IRI));
    assert!(String::from_utf8(first.bytes)
        .expect("UTF-8")
        .starts_with("org agenda — "));
    assert_eq!(first.expiry, Expiry::Always, "no clock: not cached");
    agenda.resolve(source(TODAY_IRI));
    assert!(!agenda.is_cached(&source(TODAY_IRI)));
    assert_eq!(
        agenda.file.reads(),
        before + 1,
        "the file itself is cached under its thread (`Never` needs no clock); only the \
         agenda recomputes"
    );

    let absolute = agenda.resolve(source(ABSOLUTE_IRI));
    assert_eq!(absolute.expiry, Expiry::Never, "absolute: no clock needed");
    assert!(agenda.is_cached(&source(ABSOLUTE_IRI)));
}

/// What `ikigai-conformance` 0.1.0 does not check (PENDING #11/#31/#79): a
/// declared output that is not an RDF face is never compared with what the
/// action serves, and a face served only under `as=` is never probed unless
/// declared. Both directions, by hand, at both patterns: with `as` omitted the
/// served type is a declared output, and `as`'s `one_of` IS the list of faces —
/// every value serves its own type, and every declared output is one of them.
#[test]
fn declared_outputs_are_the_media_types_served() {
    let agenda = Agenda::cacheable();
    for iri in [AGENDA_IRI, ABSOLUTE_IRI] {
        let description = agenda
            .kernel
            .describe(&Iri::parse(iri).expect("a valid IRI"))
            .unwrap_or_else(|| panic!("{iri} describes itself"));
        let spec = description
            .action_specs()
            .into_iter()
            .find(|a| a.verb == Verb::Source)
            .expect("Source is declared");
        let declared: BTreeSet<String> = spec
            .outputs
            .iter()
            .map(|o| rdf::bare_media_type(o))
            .collect();
        let faces: BTreeSet<String> = spec
            .inputs
            .iter()
            .find(|i| i.name == "as")
            .expect("`as` is declared")
            .one_of
            .iter()
            .cloned()
            .collect();
        assert_eq!(faces, declared, "{iri}: `as` lists exactly the outputs");

        let served = agenda.resolve(source(iri));
        let got = rdf::bare_media_type(&served.repr_type.media_type);
        assert_eq!(got, "text/plain", "{iri}: the default face");
        assert!(declared.contains(&got));

        for face in &faces {
            let served = agenda.resolve(request(Verb::Source, iri, &[("as", face)]));
            assert_eq!(
                rdf::bare_media_type(&served.repr_type.media_type),
                *face,
                "{iri} as={face}"
            );
        }
    }
}

/// The Turtle face, by hand and in full: parses, skolemized under
/// `urn:event:{uid}`, every term either `ical:` or one of the three `ik:`
/// properties `ikigai-vocab` defines, and every property the fixture carries
/// is on the graph. The suite's SKOLEM-RDF and VOCABULARY hold the first three
/// over the walk's minimal call; this pins the fourth.
#[test]
fn the_turtle_face_is_the_calendar_event_graph() {
    let agenda = Agenda::cacheable();
    let repr = agenda.resolve(request(
        Verb::Source,
        ABSOLUTE_IRI,
        &[("as", "text/turtle")],
    ));
    let ttl = String::from_utf8(repr.bytes.clone()).expect("UTF-8");
    let triples = rdf::parse(&repr.repr_type.media_type, &repr.bytes)
        .unwrap_or_else(|e| panic!("the Turtle face parses: {e}\n{ttl}"));
    assert!(
        triples.len() > 20,
        "a real graph, not an empty one (PENDING #26)"
    );
    assert!(rdf::blank_nodes(&triples).is_empty(), "skolemized:\n{ttl}");
    for triple in &triples {
        let subject = triple.subject.to_string();
        assert!(
            subject.starts_with("<urn:event:"),
            "every subject is a skolem event IRI: {subject}"
        );
    }
    let terms = rdf::terms(&triples);
    for term in &terms {
        assert!(rdf::is_defined(term, &[]), "`{term}` is nobody's");
        assert!(
            term.starts_with(ICAL) || term.starts_with(IK) || term.ends_with("#type"),
            "the calendar vocabulary only: {term}"
        );
    }
    for ik in ["calendar", "allDay", "alert"] {
        assert!(
            terms.contains(&format!("{IK}{ik}")),
            "ik:{ik} is on the graph"
        );
    }

    // The properties, as urn:personal:calendar reads them back.
    assert!(
        ttl.contains("<urn:event:dinner-2026-07-11> a ical:Vevent"),
        "{ttl}"
    );
    assert!(ttl.contains("ical:location \"Chez Panisse\""), "{ttl}");
    assert!(
        ttl.contains("ik:alert 60") && ttl.contains("ik:alert 1440"),
        "{ttl}"
    );
    assert!(ttl.contains("ik:allDay true"), "{ttl}");
    assert!(
        ttl.contains("ical:description \"https://us06web.zoom.us/j/1234\""),
        "the :URL: below SCHEDULED: is the entry's:\n{ttl}"
    );
    assert!(ttl.contains("ik:calendar \"calendar.org\""), "{ttl}");
    assert!(
        ttl.contains("<urn:event:org-") && ttl.contains("-2026-07-10>"),
        "a repeater occurrence is date-suffixed:\n{ttl}"
    );
}

/// The capability gate is the host's file space's, reached through
/// `inv.source`. `org-agenda` declares no scope of its own (it cannot name what
/// the host mounts), so under no grants against a gated file space the refusal
/// is the FILE's — and it reaches the caller as the typed `Denied` the kernel
/// raised, never flattened into an endpoint error. The suite cannot see a gate
/// in a sub-resolution; walked over a gated fixture it reports the over-offer,
/// which is the true shape of the composed host and is recorded here rather
/// than declared away (this crate cannot know the scope to declare).
#[test]
fn the_file_gate_is_the_hosts_and_passes_through_typed() {
    let agenda = Agenda::new(OrgFile::new(true, Some(FS_READ)), Some(noon()));
    let none = Capability::scoped(Vec::<String>::new());
    match agenda.issue(source(ABSOLUTE_IRI), &none) {
        Err(Error::Denied(msg)) => assert!(msg.contains(FS_READ), "the file's scope: {msg}"),
        other => panic!("expected the file space's typed Denied, got {other:?}"),
    }
    assert_eq!(
        agenda.file.reads(),
        0,
        "refused before the file endpoint ran"
    );
    let granted = Capability::scoped([FS_READ]);
    let text = String::from_utf8(
        agenda
            .issue(source(ABSOLUTE_IRI), &granted)
            .expect("a reader with the file scope")
            .bytes,
    )
    .expect("UTF-8");
    assert!(text.contains("Dinner with the Hendersons"), "{text}");

    let report = suite().run_blocking(&agenda.kernel);
    eprintln!("[gated file space]\n{report}");
    assert_eq!(
        findings(&report),
        [
            (AGENDA, Some(Verb::Source), Check::Enforced),
            (AGENDA, Some(Verb::Source), Check::Enforced),
        ],
        "the agenda over-offers by exactly the file's scope, at both patterns: {report}"
    );
    assert!(
        report
            .findings
            .iter()
            .all(|f| f.detail.contains("declares no capability") && f.detail.contains(FS_READ)),
        "{report}"
    );
}

/// `Fixture::new(id, …)` is looked up by description id; an id that matches no
/// description is silently unused. The ids this file uses are held to what the
/// kernel serves at every pattern.
#[test]
fn the_fixture_ids_are_the_description_ids() {
    let agenda = Agenda::cacheable();
    for (iri, id) in [
        (AGENDA_IRI, AGENDA),
        (ABSOLUTE_IRI, AGENDA),
        (FILE_IRI, ORGFILE),
    ] {
        let description = agenda
            .kernel
            .describe(&Iri::parse(iri).expect("a valid IRI"))
            .unwrap_or_else(|| panic!("{iri} describes itself"));
        assert_eq!(description.id, id, "{iri}");
    }
}

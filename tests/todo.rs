//! The TODO doors over the real file space: `ikigai_org::todo_space` beside an
//! `ikigai-fs` `FileEndpoint` at `urn:orgfile:{path}`, on real files in a scratch root.
//! The Sink's promise rests on that endpoint's compare-and-replace, so nothing here
//! stands in for it, except [`IgnoresIfMatch`], the file space the Sink must refuse.
//!
//! "The way Emacs would" means `std::fs::write` on the file, outside the kernel, followed
//! on a cacheable mount by the cut a host's watcher makes.
//!
//! `Suite::own_all` keeps the walk to this crate's five doors: `ikigai-fs` walks its own
//! endpoint in its own repository, and a walk here would fire its Sink at the fixture.
//! No opt-outs. `BODY-INPUTS` has nothing to probe, because `content` is not declared
//! `reads_body`: repeating one append is rightly a `Conflict`, so its answers could never
//! compare. `REQUIRED` covers `id` and `title` only by [`required_inputs_are_refused`],
//! for the same reason.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::executor::block_on;
use ikigai_conformance::race::{Call, Race};
use ikigai_conformance::{rdf, Check, Fixture, Report, Suite};
use ikigai_core::{
    ArgRef, ArgSpec, Body, Capability, Description, Endpoint, EndpointSpace, Error, Expiry,
    Fallback, Invocation, Iri, Kernel, ReprType, Representation, Request, Result, Space,
    UriTemplate, Verb,
};
use ikigai_fs::FileEndpoint;
use ikigai_org::{todo_space, TodoFile};

const QUEUE: &str = "queue.org";
const QUEUE_IRI: &str = "urn:orgfile:queue.org";
const SINK: &str = "urn:org:todo:file:queue";
const TODO_DOORS: [&str; 5] = [
    "org-todo",
    "org-todo-id",
    "org-todo-state",
    "org-todo-tag",
    "org-todo-file",
];
const XSD_STRING: &str = "http://www.w3.org/2001/XMLSchema#string";
const ICAL: &str = "http://www.w3.org/2002/12/cal/ical#";
const IK: &str = "https://ikigai-rs.dev/ns#";

/// The hub's queue: its own keywords, a stable id on every entry, a body with a link and
/// a src block.
const ORG: &str = "\
#+TITLE: What waits on Brian
#+TODO: TODO NEXT WAIT | DONE CANCELED

* Queue
** NEXT Read the draft                                                :review:hub:
:PROPERTIES:
:ID:       draft-1
:END:

See [[https://example.com/draft][the draft]].

#+BEGIN_SRC sh
  * an indented star, inside a block
#+END_SRC

** WAIT Sign the form                                                       :legal:
:PROPERTIES:
:ID:       form-1
:END:

* Reference
Notes that are nobody's TODO.
";

struct Root(PathBuf);

impl Root {
    fn new(name: &str) -> Root {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "ikigai-org-todo-{}-{name}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join(QUEUE), ORG).unwrap();
        Root(dir)
    }

    fn read(&self, name: &str) -> String {
        std::fs::read_to_string(self.0.join(name)).unwrap()
    }

    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.0.join(name), text).unwrap();
    }

    /// Every name in the root: a staging file would show here.
    fn names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(&self.0)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Root {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

fn file_space(endpoint: impl Endpoint + 'static) -> Arc<dyn Space> {
    Arc::new(EndpointSpace::new().bind(
        UriTemplate::parse("urn:orgfile:{path}").expect("a valid template"),
        endpoint,
    ))
}

/// The kernel under test over `files`, and the TODO space's own `Arc`s for the suite.
struct Todo {
    kernel: Kernel,
    org: Arc<EndpointSpace>,
    root: Arc<Fallback>,
}

fn todo_over(files: Arc<dyn Space>, bound: Vec<TodoFile>) -> Todo {
    let org = Arc::new(todo_space(bound));
    let root = Arc::new(Fallback::new(vec![files, org.clone()]));
    Todo {
        kernel: Kernel::new(root.clone()),
        org,
        root,
    }
}

fn queue() -> Vec<TodoFile> {
    vec![TodoFile::new("queue", QUEUE_IRI)]
}

/// The host's golden-thread-ready mount.
fn cacheable(root: &Root) -> Todo {
    todo_over(file_space(FileEndpoint::new(&root.0).cacheable()), queue())
}

/// The host's default mount: every read is live.
fn live(root: &Root) -> Todo {
    todo_over(file_space(FileEndpoint::new(&root.0)), queue())
}

fn request(verb: Verb, iri: &str, args: &[(&str, &str)]) -> Request {
    let mut request = Request::new(verb, Iri::parse(iri).expect("a valid IRI"));
    for (name, value) in args {
        request = request.with_arg(*name, ArgRef::Inline(value.as_bytes().to_vec()));
    }
    request
}

impl Todo {
    fn issue(&self, request: Request, capability: &Capability) -> Result<Representation> {
        block_on(self.kernel.issue(request, capability))
    }

    fn text(&self, iri: &str) -> String {
        let answer = self
            .issue(request(Verb::Source, iri, &[]), &Capability::root())
            .unwrap_or_else(|e| panic!("{iri}: {e}"));
        String::from_utf8(answer.bytes).expect("UTF-8")
    }

    fn append(&self, args: &[(&str, &str)]) -> Result<Representation> {
        self.issue(request(Verb::Sink, SINK, args), &Capability::root())
    }
}

fn conflict(result: Result<Representation>) -> String {
    match result {
        Err(Error::Conflict(detail)) => detail,
        other => panic!("expected Conflict, got {other:?}"),
    }
}

/// The suite for this module's doors, with fixtures naming what the file holds.
fn suite(todo: &Todo) -> Suite {
    Suite::new()
        .own_all(TODO_DOORS)
        .body_space(todo.root.clone())
        .fixture(Fixture::new("org-todo-id", Verb::Source).binding("id", "draft-1"))
        .fixture(Fixture::new("org-todo-state", Verb::Source).binding("state", "WAIT"))
        .fixture(Fixture::new("org-todo-tag", Verb::Source).binding("tag", "legal"))
        .fixture(
            Fixture::new("org-todo-file", Verb::Sink)
                .binding("name", "queue")
                .arg("id", "walked-1")
                .arg("title", "Walked by the suite"),
        )
        .host_named_space("ikigai_org::todo_space(files)", todo.org.clone())
}

fn assert_shape(report: &Report) {
    assert_eq!(&*report.walked, TODO_DOORS, "{report}");
    assert_eq!(
        report.checks.skipped().collect::<Vec<_>>(),
        Vec::<Check>::new(),
        "every check runs: {report}"
    );
}

#[test]
fn conforms_over_a_cacheable_file_space() {
    let root = Root::new("cacheable");
    let todo = cacheable(&root);
    let report = suite(&todo)
        .cacheable("org-todo")
        .cacheable("org-todo-id")
        .cacheable("org-todo-state")
        .cacheable("org-todo-tag")
        .run_blocking(&todo.kernel);
    eprintln!("[cacheable file space]\n{report}");
    assert!(report.is_clean(), "{report}");
    assert_shape(&report);
    assert!(
        root.read(QUEUE).contains("Walked by the suite"),
        "the walk fired the Sink, through the real conditional write"
    );
}

#[test]
fn conforms_over_a_live_file_space() {
    let root = Root::new("live");
    let todo = live(&root);
    let report = suite(&todo).run_blocking(&todo.kernel);
    eprintln!("[live file space]\n{report}");
    assert!(report.is_clean(), "{report}");
    assert_shape(&report);
    let read = todo
        .issue(
            request(Verb::Source, "urn:org:todo", &[]),
            &Capability::root(),
        )
        .unwrap();
    assert_eq!(read.expiry, Expiry::Always, "live file, live entries");
}

/// The deliverable's round trip: append, read by id, change the state in the text the way
/// Emacs would, read the new state and everything Emacs logged with it.
#[test]
fn an_appended_entry_round_trips_through_an_edit_in_emacs() {
    let root = Root::new("round-trip");
    let todo = cacheable(&root);
    todo.append(&[
        ("id", "new-1"),
        ("title", "File the report"),
        ("state", "NEXT"),
        ("tags", "hub, urgent"),
        ("content", "Body with [[https://example.com][a link]]."),
    ])
    .unwrap();

    let read = todo.text("urn:org:todo:id:new-1");
    assert_eq!(
        read,
        "org todo — new-1\n\nstate: NEXT\ntitle: File the report\ntags: hub, urgent\n\
         file: queue\nclosed: (none)\nlogbook: (none)\n\
         body:\nBody with [[https://example.com][a link]].\n"
    );

    // Emacs changes the keyword, adds CLOSED and a state note, and saves.
    let edited = root
        .read(QUEUE)
        .replace("* NEXT File the report", "* DONE File the report");
    let edited = edited.replace(
        ":PROPERTIES:\n:ID:       new-1\n:END:\n",
        "CLOSED: [2026-11-04 Wed 16:20]\n:PROPERTIES:\n:ID:       new-1\n:END:\n\
         :LOGBOOK:\n- State \"DONE\"       from \"NEXT\"       [2026-11-04 Wed 16:20] \\\\\n  \
         filed it\n:END:\n",
    );
    root.write(QUEUE, &edited);
    // What a host's watcher does when the file changes on disk.
    todo.kernel.cut(QUEUE_IRI);

    let read = todo.text("urn:org:todo:id:new-1");
    assert!(read.contains("state: DONE\n"), "{read}");
    assert!(read.contains("closed: 2026-11-04 Wed 16:20\n"), "{read}");
    assert!(
        read.contains("logbook:\n  State \"DONE\" from \"NEXT\" [2026-11-04 Wed 16:20] filed it\n"),
        "{read}"
    );
    assert!(
        read.ends_with("[[https://example.com][a link]].\n"),
        "{read}"
    );
    let done = todo.text("urn:org:todo:state:DONE");
    assert!(done.contains("File the report"), "{done}");
    let next = todo.text("urn:org:todo:state:NEXT");
    assert!(!next.contains("File the report"), "{next}");
}

/// Stands where the file endpoint stands and runs `edit` just before the first Sink
/// reaches it: an edit landing between the read and the write, every time.
struct EditBeforeSink {
    inner: FileEndpoint,
    edit: Mutex<Option<Box<dyn FnOnce() + Send>>>,
}

impl EditBeforeSink {
    fn edit_if_writing(&self, inv: &Invocation<'_>) {
        if inv.request.verb == Verb::Sink {
            if let Some(edit) = self.edit.lock().unwrap().take() {
                edit();
            }
        }
    }
}

#[async_trait::async_trait]
impl Endpoint for EditBeforeSink {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        self.edit_if_writing(inv);
        self.inner.invoke(inv).await
    }

    // The kernel's own path: the endpoint's `invoke` is not called through a wrapper's.
    async fn invoke_body(&self, inv: &Invocation<'_>) -> Result<Representation<Body>> {
        self.edit_if_writing(inv);
        self.inner.invoke_body(inv).await
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn describe(&self) -> Description {
        self.inner.describe()
    }
}

/// An edit saved in Emacs between the Sink's read and its write: refused, the edit kept,
/// nothing appended, no staging file left, and the retry goes through onto the edit.
#[test]
fn an_edit_between_the_read_and_the_write_is_a_conflict_and_is_kept() {
    let root = Root::new("concurrent");
    let saved = ORG.replace("** WAIT Sign the form", "** DONE Sign the form");
    let edit = {
        let path = root.0.join(QUEUE);
        let saved = saved.clone();
        Box::new(move || std::fs::write(path, saved).unwrap())
    };
    let todo = todo_over(
        file_space(EditBeforeSink {
            inner: FileEndpoint::new(&root.0),
            edit: Mutex::new(Some(edit)),
        }),
        queue(),
    );

    let detail = conflict(todo.append(&[("id", "new-1"), ("title", "Lost?")]));
    assert!(detail.contains(QUEUE_IRI), "{detail}");
    assert!(detail.contains("read it again"), "{detail}");
    assert_eq!(root.read(QUEUE), saved, "the edit stands, byte for byte");
    assert_eq!(root.names(), [QUEUE], "no staging file");

    todo.append(&[("id", "new-1"), ("title", "Lost?")]).unwrap();
    let now = root.read(QUEUE);
    assert!(now.starts_with(&saved), "{now}");
    assert!(now.contains("* TODO Lost?\n"), "{now}");
}

/// Many writers at once, none of them told it succeeded unless its entry is in the file.
/// A race is a rate: with `if-match` taken off the write, this load lost an acknowledged
/// entry in 4, 1 and 5 of 300 rounds on three runs (measured 2026-11-04), so it goes red
/// on that defect. The forced interleaving above is the regression test that always does.
#[test]
fn racing_appends_never_lose_one_that_succeeded() {
    let root = Root::new("race");
    let todo = live(&root);
    let round = AtomicUsize::new(0);
    Race::over(&todo.kernel)
        .rounds(300)
        .together(
            |_kernel, _round| {
                let n = round.fetch_add(1, Ordering::Relaxed);
                (n, root.read(QUEUE))
            },
            |(n, _)| {
                (0..8)
                    .map(|w| {
                        let id = format!("r{n}-w{w}");
                        Call::new(Verb::Sink, SINK).arg("id", &id).arg("title", &id)
                    })
                    .collect()
            },
            |_kernel, (n, before), answers| {
                let after = root.read(QUEUE);
                if !after.starts_with(before.as_str()) {
                    return Err("an existing byte changed".to_string());
                }
                for (w, answer) in answers.iter().enumerate() {
                    let held = after.contains(&format!(":ID:       r{n}-w{w}\n"));
                    match answer {
                        Ok(_) if !held => {
                            return Err(format!("r{n}-w{w} was told it succeeded and is gone"))
                        }
                        Err(Error::Conflict(_) | Error::Unavailable(_) | Error::Endpoint(_))
                            if held =>
                        {
                            return Err(format!("r{n}-w{w} was refused and landed"))
                        }
                        Err(Error::Conflict(_) | Error::Unavailable(_) | Error::Endpoint(_))
                        | Ok(_) => {}
                        Err(other) => return Err(format!("r{n}-w{w}: {other}")),
                    }
                }
                Ok(())
            },
        )
        .assert_holds();
    assert_eq!(root.names(), [QUEUE], "no staging file");
}

#[test]
fn an_id_already_present_is_a_conflict_wherever_it_is() {
    let root = Root::new("duplicate");
    root.write(
        "other.org",
        "* A plain heading\n:PROPERTIES:\n:ID:       plain-elsewhere\n:END:\n",
    );
    let todo = todo_over(
        file_space(FileEndpoint::new(&root.0)),
        vec![
            TodoFile::new("queue", QUEUE_IRI),
            TodoFile::new("other", "urn:orgfile:other.org"),
        ],
    );
    let before = root.read(QUEUE);
    for id in ["draft-1", "plain-elsewhere"] {
        let detail = conflict(todo.append(&[("id", id), ("title", "Again")]));
        assert!(
            detail.contains(&format!("urn:org:todo:id:{id} already exists")),
            "{detail}"
        );
        assert!(detail.contains("nothing was written"), "{detail}");
    }
    assert_eq!(root.read(QUEUE), before);

    // The same append twice: the second finds the first.
    todo.append(&[("id", "new-1"), ("title", "Once")]).unwrap();
    conflict(todo.append(&[("id", "new-1"), ("title", "Once")]));
    assert_eq!(root.read(QUEUE).matches("* TODO Once").count(), 1);
}

/// The file's own `#+TODO:` decides what is an entry, what a Sink may write, and the
/// default state; org's default does not leak in.
#[test]
fn a_custom_todo_line_is_the_files_vocabulary() {
    let root = Root::new("keywords");
    root.write(
        QUEUE,
        "#+TODO: ASK(a) BLOCKED(b@/!) | ANSWERED(n!)\n\n* ASK First\n* TODO Not a keyword here\n",
    );
    let todo = live(&root);
    let list = todo.text("urn:org:todo");
    assert!(list.contains("  ASK  First  :ID: org-"), "{list}");
    assert!(!list.contains("Not a keyword"), "{list}");

    todo.append(&[("id", "a-1"), ("title", "Defaults")])
        .unwrap();
    assert!(todo.text("urn:org:todo:id:a-1").contains("state: ASK\n"));

    todo.append(&[("id", "a-2"), ("title", "Blocked"), ("state", "BLOCKED")])
        .unwrap();
    let blocked = todo.text("urn:org:todo:state:BLOCKED");
    assert!(blocked.contains("Blocked  :ID: a-2"), "{blocked}");

    let refused = todo.append(&[("id", "a-3"), ("title", "No"), ("state", "TODO")]);
    match refused {
        Err(Error::InvalidArgument { name, detail }) => {
            assert_eq!(name, "state");
            assert!(
                detail.contains("whose keywords are ASK, BLOCKED, ANSWERED"),
                "{detail}"
            );
        }
        other => panic!("expected InvalidArgument, got {other:?}"),
    }
}

/// `under` files a child beneath a top-level heading, and that insertion is the whole
/// change: everything before and after it is the old bytes.
#[test]
fn under_inserts_one_entry_and_changes_nothing_else() {
    let root = Root::new("under");
    let todo = live(&root);
    todo.append(&[
        ("id", "new-1"),
        ("title", "Filed under"),
        ("under", "Queue"),
        ("tags", "hub"),
        (
            "content",
            "Line one.\n\n#+BEGIN_SRC sh\necho hi\n#+END_SRC\n",
        ),
    ])
    .unwrap();
    let after = root.read(QUEUE);
    let inserted = after.find("** TODO Filed under").expect("inserted");
    let end = after[inserted..].find("#+END_SRC\n").unwrap() + inserted + "#+END_SRC\n".len();
    assert_eq!(format!("{}{}", &after[..inserted], &after[end..]), ORG);
    assert!(
        after[..inserted].ends_with(":ID:       form-1\n:END:\n"),
        "at the end of the subtree: {after}"
    );
    assert!(after[end..].starts_with("\n* Reference\n"), "{after}");
    assert!(after[inserted..end].contains("\n:ID:       new-1\n:END:\n\nLine one.\n"));
    let read = todo.text("urn:org:todo:id:new-1");
    assert!(
        read.ends_with("body:\nLine one.\n\n#+BEGIN_SRC sh\necho hi\n#+END_SRC\n"),
        "{read}"
    );

    let detail = conflict(todo.append(&[("id", "x"), ("title", "No"), ("under", "Nowhere")]));
    assert!(detail.contains("\"Nowhere\""), "{detail}");
}

/// A file space that ignores `if-match`, as `ikigai-fs` 0.4.0 does: it writes whatever it
/// is given, so writing through it would lose an edit. The Sink must not.
struct IgnoresIfMatch {
    text: Mutex<String>,
    writes: AtomicUsize,
}

#[async_trait::async_trait]
impl Endpoint for IgnoresIfMatch {
    async fn invoke(&self, inv: &Invocation<'_>) -> Result<Representation> {
        if inv.request.verb == Verb::Sink {
            self.writes.fetch_add(1, Ordering::SeqCst);
            *self.text.lock().unwrap() = inv.inline_str("content")?.to_string();
        }
        let text = self.text.lock().unwrap().clone();
        Ok(Representation::new(
            ReprType::new("text/plain"),
            text.into_bytes(),
        ))
    }

    fn name(&self) -> &str {
        "orgfile"
    }

    fn describe(&self) -> Description {
        Description::new("orgfile")
            .verb(Verb::Source)
            .verb(Verb::Sink)
            .input(ArgSpec::new("path").binding().class(XSD_STRING))
            .input(ArgSpec::new("content").class(XSD_STRING).optional())
            .output("text/plain")
    }
}

#[test]
fn a_file_space_with_no_conditional_write_is_refused_before_anything_is_written() {
    let file = Arc::new(IgnoresIfMatch {
        text: Mutex::new(ORG.to_string()),
        writes: AtomicUsize::new(0),
    });
    let files = EndpointSpace::new().bind_arc(
        UriTemplate::parse("urn:orgfile:{path}").expect("a valid template"),
        file.clone(),
    );
    let todo = todo_over(Arc::new(files), queue());
    match todo.append(&[("id", "new-1"), ("title", "Blind")]) {
        Err(Error::Unsupported { verb, detail }) => {
            assert_eq!(verb, Some(Verb::Sink));
            assert!(detail.contains(QUEUE_IRI), "{detail}");
            assert!(detail.contains("nothing was written"), "{detail}");
            assert!(
                detail.contains("ikigai-fs serves one from 0.4.1"),
                "{detail}"
            );
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
    assert_eq!(file.writes.load(Ordering::SeqCst), 0);
    assert_eq!(*file.text.lock().unwrap(), ORG);
}

/// Declared equals enforced, in all four cells: the read door under the read scope, the
/// Sink under read and write, and the file space's own path rule still judging the grant.
#[test]
fn capabilities_are_the_file_spaces_per_verb() {
    let root = Root::new("capabilities");
    let todo = live(&root);
    let dir = root.0.display().to_string();
    let read = format!("urn:cap:fs:read:{dir}");
    let write = format!("urn:cap:fs:write:{dir}");
    let reader = Capability::scoped([read.clone()]);
    let writer_only = Capability::scoped([write.clone()]);
    let both = Capability::scoped([read, write]);
    let none = Capability::scoped(Vec::<String>::new());
    let list = || request(Verb::Source, "urn:org:todo", &[]);
    let add = |id: &str| request(Verb::Sink, SINK, &[("id", id), ("title", "Granted")]);
    let before = root.read(QUEUE);

    assert!(matches!(todo.issue(list(), &none), Err(Error::Denied(_))));
    assert!(todo.issue(list(), &reader).is_ok());
    assert!(matches!(todo.issue(add("a"), &none), Err(Error::Denied(_))));
    assert!(matches!(
        todo.issue(add("a"), &reader),
        Err(Error::Denied(_))
    ));
    assert!(matches!(
        todo.issue(add("a"), &writer_only),
        Err(Error::Denied(_))
    ));
    assert_eq!(
        root.read(QUEUE),
        before,
        "no refused write touched the file"
    );
    todo.issue(add("a"), &both).expect("read + write appends");
    assert!(root.read(QUEUE).contains(":ID:       a\n"));

    let elsewhere = Capability::scoped(["urn:cap:fs:read:/nowhere", "urn:cap:fs:write:/nowhere"]);
    assert!(
        matches!(todo.issue(add("b"), &elsewhere), Err(Error::Denied(_))),
        "the floor admits any grant in the family; the path rule refuses this one"
    );
    assert!(!root.read(QUEUE).contains(":ID:       b\n"));
}

/// Reads hang on the file's thread: a Sink through this crate cuts it, so the next read
/// sees the entry, and until then the answer comes from the cache.
#[test]
fn a_read_is_cached_under_the_file_and_an_append_cuts_it() {
    let root = Root::new("threads");
    let todo = cacheable(&root);
    let list = request(Verb::Source, "urn:org:todo", &[]);
    let first = todo.issue(list.clone(), &Capability::root()).unwrap();
    assert_eq!(first.expiry, Expiry::Never, "a function of the file");
    assert!(
        first.threads().iter().any(|t| t.to_string() == QUEUE_IRI),
        "under the file's thread"
    );
    assert!(todo.kernel.is_cached(&list, &Capability::root()));

    // An edit nothing has cut for: stale by design, as for the agenda…
    root.write(QUEUE, &format!("{ORG}\n* TODO Saved in Emacs\n"));
    assert!(!todo.text("urn:org:todo").contains("Saved in Emacs"));
    // …and the Sink's read is that same stale read, so it refuses, rather than lose it.
    conflict(todo.append(&[("id", "new-1"), ("title", "Stale")]));
    assert!(root.read(QUEUE).contains("Saved in Emacs"));
    assert!(!root.read(QUEUE).contains("Stale"));

    todo.kernel.cut(QUEUE_IRI);
    todo.append(&[("id", "new-1"), ("title", "Fresh")]).unwrap();
    let list = todo.text("urn:org:todo");
    assert!(list.contains("Saved in Emacs"), "{list}");
    assert!(
        list.contains("Fresh  :ID: new-1"),
        "the append cut the read: {list}"
    );
}

#[test]
fn the_text_faces_and_partitions() {
    let root = Root::new("faces");
    let todo = live(&root);
    assert_eq!(
        todo.text("urn:org:todo"),
        "org todo — all entries\n\n  \
         NEXT  Read the draft  :ID: draft-1  tags: review, hub\n  \
         WAIT  Sign the form  :ID: form-1  tags: legal\n"
    );
    assert_eq!(
        todo.text("urn:org:todo:state:WAIT"),
        "org todo — state WAIT\n\n  WAIT  Sign the form  :ID: form-1  tags: legal\n"
    );
    assert_eq!(
        todo.text("urn:org:todo:tag:hub"),
        "org todo — tag hub\n\n  NEXT  Read the draft  :ID: draft-1  tags: review, hub\n"
    );
    assert_eq!(
        todo.text("urn:org:todo:tag:absent"),
        "org todo — tag absent\n\n  (no entries)\n"
    );
    let one = todo.text("urn:org:todo:id:draft-1");
    assert!(
        one.ends_with(
            "body:\nSee [[https://example.com/draft][the draft]].\n\n#+BEGIN_SRC sh\n  \
             * an indented star, inside a block\n#+END_SRC\n"
        ),
        "{one}"
    );
    match todo.issue(
        request(Verb::Source, "urn:org:todo:id:absent", &[]),
        &Capability::root(),
    ) {
        Err(Error::NotFound(detail)) => assert!(detail.contains("no entry has this id")),
        other => panic!("expected NotFound, got {other:?}"),
    }
}

/// Two entries claiming one id cannot be told apart by asking for it: say so, naming the
/// files, rather than answer with either.
#[test]
fn an_id_two_entries_claim_is_refused_by_name() {
    let root = Root::new("claimed");
    let twice =
        "* TODO One\n:PROPERTIES:\n:ID: same\n:END:\n* TODO Two\n:PROPERTIES:\n:ID: same\n:END:\n";
    root.write(QUEUE, twice);
    let todo = live(&root);
    let detail = conflict(todo.issue(
        request(Verb::Source, "urn:org:todo:id:same", &[]),
        &Capability::root(),
    ));
    assert!(
        detail.contains("2 entries claim this id, in queue, queue"),
        "{detail}"
    );
    let ttl = String::from_utf8(
        todo.issue(
            request(Verb::Source, "urn:org:todo", &[("as", "text/turtle")]),
            &Capability::root(),
        )
        .unwrap()
        .bytes,
    )
    .unwrap();
    assert_eq!(ttl.matches("<urn:org:todo:id:same>").count(), 1, "{ttl}");
}

/// The Turtle face in full: parses, skolemized under `urn:org:todo:id:`, an entry with no
/// `:ID:` under its stable FNV-1a id, only `ical:` and the vocabulary's `ik:calendar`.
#[test]
fn the_turtle_face_is_skolemized_on_defined_terms() {
    let root = Root::new("turtle");
    root.write(
        QUEUE,
        &format!(
            "{ORG}\n* DONE No id here :x:\nCLOSED: [2026-11-04 Wed 10:00]\n\nSome \"body\"\nline.\n"
        ),
    );
    let todo = live(&root);
    let repr = todo
        .issue(
            request(Verb::Source, "urn:org:todo", &[("as", "text/turtle")]),
            &Capability::root(),
        )
        .unwrap();
    let ttl = String::from_utf8(repr.bytes.clone()).unwrap();
    let triples = rdf::parse(&repr.repr_type.media_type, &repr.bytes)
        .unwrap_or_else(|e| panic!("the Turtle face parses: {e}\n{ttl}"));
    assert!(rdf::blank_nodes(&triples).is_empty(), "{ttl}");
    for triple in &triples {
        let subject = triple.subject.to_string();
        assert!(subject.starts_with("<urn:org:todo:id:"), "{subject}");
    }
    for term in rdf::terms(&triples) {
        assert!(rdf::is_defined(&term, &[]), "`{term}` is nobody's");
        assert!(
            term.starts_with(ICAL) || term == format!("{IK}calendar") || term.ends_with("#type"),
            "{term}"
        );
    }
    assert!(
        ttl.contains("<urn:org:todo:id:draft-1> a ical:Vtodo"),
        "{ttl}"
    );
    assert!(ttl.contains("ical:status \"NEXT\""), "{ttl}");
    assert!(
        ttl.contains("ical:categories \"review\"") && ttl.contains("ical:categories \"hub\""),
        "{ttl}"
    );
    assert!(ttl.contains("<urn:org:todo:id:org-"), "{ttl}");
    assert!(
        ttl.contains("ical:completed \"2026-11-04T10:00:00\""),
        "{ttl}"
    );
    assert!(
        ttl.contains("ical:description \"Some \\\"body\\\"\\nline.\""),
        "{ttl}"
    );
    assert!(ttl.contains("ik:calendar \"queue\""), "{ttl}");
    let derived = ttl
        .split("<urn:org:todo:id:")
        .find(|s| s.starts_with("org-"))
        .and_then(|s| s.split('>').next())
        .unwrap();
    let read = todo.text(&format!("urn:org:todo:id:{derived}"));
    assert!(
        read.contains("title: No id here\n"),
        "the subject resolves: {read}"
    );
}

/// What the suite's `REQUIRED` would hold if it fired mutating actions (it does not
/// repeat them unasked), and the refusals' words.
#[test]
fn required_inputs_are_refused() {
    let root = Root::new("required");
    let todo = live(&root);
    let before = root.read(QUEUE);
    assert!(matches!(
        todo.append(&[("title", "No id")]),
        Err(Error::MissingArgument(name)) if name == "id"
    ));
    assert!(matches!(
        todo.append(&[("id", "x")]),
        Err(Error::MissingArgument(name)) if name == "title"
    ));
    for (args, input, words) in [
        (
            vec![("id", "a b"), ("title", "T")],
            "id",
            "cannot name urn:org:todo:id:{id}",
        ),
        (
            vec![("id", "x"), ("title", "T :tag:")],
            "title",
            "ends in what org reads as tags",
        ),
        (
            vec![("id", "x"), ("title", "T"), ("tags", "a-b")],
            "tags",
            "is not an org tag",
        ),
        (
            vec![("id", "x"), ("title", "T"), ("content", "* New headline")],
            "content",
            "would start a new org entry",
        ),
        (
            vec![("id", "x"), ("title", "T"), ("content", "#+TODO: A | B")],
            "content",
            "would change the TODO keywords",
        ),
    ] {
        match todo.append(&args) {
            Err(Error::InvalidArgument { name, detail }) => {
                assert_eq!(name, input, "{detail}");
                assert!(detail.contains(words), "{detail}");
            }
            other => panic!("{input}: expected InvalidArgument, got {other:?}"),
        }
    }
    match todo.issue(
        request(
            Verb::Sink,
            "urn:org:todo:file:absent",
            &[("id", "x"), ("title", "T")],
        ),
        &Capability::root(),
    ) {
        Err(Error::NotFound(detail)) => {
            assert!(detail.contains("no bound org file"), "{detail}");
            assert!(detail.contains("bound: queue"), "{detail}");
        }
        other => panic!("expected NotFound, got {other:?}"),
    }
    assert_eq!(root.read(QUEUE), before);
}

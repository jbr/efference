//! `status`, `transcript`, `interrupt` and `archive` in-session: what each reader is
//! told, and the cursor the host holds per reader (docs/tools.md).

use super::*;
use crate::control_tools::Transcript;

fn status(agent: Option<&str>) -> Status {
    Status {
        agent: agent.map(str::to_string),
    }
}

/// A gate routed to the reader reads as its decision, with the call that decides it;
/// the same gate read by a session that drives the target but is not its reviewer
/// names who decides; a session that does not drive it reads no move at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_status_reads_as_the_callers_relation() {
    let fixture = Fixture::new();
    let child = fixture.session("child");
    let seq = issue(&child, vec![curl()]);
    let _gate = child.register_pending(&curl(), None, None);

    let reviewer = status(Some("child"))
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .unwrap();

    assert!(
        reviewer.starts_with(&format!(
            "child: waiting for your decision on bash `curl -sI https://example.com` (seq {seq})\n\
             intent: fetch only the response headers\n\
             decide: agents__decide(agent: \"child\", seq: \"{seq}\", decision: \"allow\" | \
             \"deny\", message?)"
        )),
        "{reviewer}"
    );

    let observer = status(Some("child"))
        .execute(fixture.control("peer"), RequestContext::default())
        .await
        .unwrap();
    assert!(
        observer.starts_with("child: waiting for a decision from parent on bash"),
        "{observer}"
    );
    assert!(!observer.contains("agents__"), "{observer}");

    // The grandchild's gate goes to the child, its supervisor, and wakes it; the parent
    // drives the grandchild through the creation chain but does not review it.
    let grandchild = fixture.session("grandchild");
    issue(&grandchild, vec![curl()]);
    let _gate = grandchild.register_pending(&curl(), None, None);
    let driver = status(Some("grandchild"))
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .unwrap();
    assert!(
        driver.starts_with("grandchild: waiting for a decision from child on bash"),
        "{driver}"
    );
    assert!(!driver.contains("decide:"), "{driver}");

    fixture.host.swansong().shut_down().await;
}

/// With no agent, one line per session in the caller's project, the caller left out
/// and the sessions it drives marked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_overview_marks_the_sessions_you_drive() {
    let fixture = Fixture::new();
    let overview = status(None)
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .unwrap();
    assert_eq!(
        overview,
        "child (yours): idle, its turn is done\n\
         grandchild (yours): idle, its turn is done\n\
         peer: idle, its turn is done"
    );

    fixture.host.swansong().shut_down().await;
}

/// A project with no other agent says so and names the in-session spelling of the verb
/// that starts one; a session addressing itself is told what would work, and no
/// refusal sends the reader to a doc.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_empty_overview_and_a_refusal_name_what_works() {
    let fixture = Fixture::new();
    let overview = status(None)
        .execute(fixture.control("stranger"), RequestContext::default())
        .await
        .unwrap();
    assert_eq!(
        overview,
        "no other agents in this project; `agents__create` starts one"
    );

    let itself = status(Some("parent"))
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(
        itself,
        "refused: \"parent\" is you — a session cannot address itself; name another agent, \
         as `agents__status` lists them"
    );

    fixture.host.swansong().shut_down().await;
}

fn read(seq: Option<&str>) -> Transcript {
    Transcript {
        agent: "child".into(),
        seq: seq.map(str::to_string),
        since: None,
        tail: None,
        audit: false,
    }
}

/// The host holds a cursor per reader and target: a first read is the tail and says
/// so, the next is what is new, a zoom leaves the cursor alone, and another reader
/// starts from its own first read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_reader_has_its_own_cursor() {
    let fixture = Fixture::new();
    let child = fixture.session("child");
    child.append(1, Payload::user("first")).unwrap();
    let parent = fixture.control("parent");
    let ctx = RequestContext::default;

    let first = read(None).execute(parent.clone(), ctx()).await.unwrap();
    assert!(
        first.starts_with("transcript of child, seq 0") && first.contains("(your first read)"),
        "{first}"
    );
    let again = read(None).execute(parent.clone(), ctx()).await.unwrap();
    assert!(
        again.starts_with("transcript of child: nothing new since your last read"),
        "{again}"
    );

    let next = child.append(1, Payload::user("second")).unwrap();
    let zoom = read(Some(&next.to_string()))
        .execute(parent.clone(), ctx())
        .await
        .unwrap();
    assert!(zoom.contains("second"), "{zoom}");
    let new = read(None).execute(parent.clone(), ctx()).await.unwrap();
    assert!(
        new.starts_with(&format!(
            "transcript of child, new since your last read: seq {next}"
        )),
        "the zoom did not move the cursor: {new}"
    );
    assert!(new.contains("second") && !new.contains("first"), "{new}");

    let peer = read(None)
        .execute(fixture.control("peer"), ctx())
        .await
        .unwrap();
    assert!(peer.contains("(your first read)"), "{peer}");
    assert!(peer.contains("first") && peer.contains("second"), "{peer}");

    fixture.host.swansong().shut_down().await;
}

/// `interrupt` says what it stopped, and at an idle session signals nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn interrupt_says_what_it_stopped() {
    let fixture = Fixture::new();
    let child = fixture.session("child");
    let interrupt = || Interrupt {
        agent: "child".into(),
    };
    let interrupts = |session: &Session| {
        session
            .snapshot()
            .iter()
            .filter(|e| {
                matches!(
                    &e.payload,
                    Payload::Telemetry(crate::log::Telemetry::Interrupt(_))
                )
            })
            .count()
    };
    let said = interrupt()
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .unwrap();
    assert_eq!(said, "nothing to interrupt: child is idle");
    assert_eq!(interrupts(&child), 0, "nothing was signalled");

    child.set_phase(LoopPhase::Requesting);
    let said = interrupt()
        .execute(fixture.control("parent"), RequestContext::default())
        .await
        .unwrap();
    assert!(said.starts_with("interrupted child"), "{said}");
    assert_eq!(interrupts(&child), 1);

    fixture.host.swansong().shut_down().await;
}

/// `archive` names the sessions that ended with it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_names_what_ended_with_it() {
    let fixture = Fixture::new();
    let said = Archive {
        agent: "child".into(),
    }
    .execute(fixture.control("parent"), RequestContext::default())
    .await
    .unwrap();
    assert_eq!(
        said,
        "archived child and the session it created (grandchild); logs stay on disk"
    );
    wait_for("the cascade", || {
        fixture.host.session("grandchild").is_none()
    })
    .await;

    fixture.host.swansong().shut_down().await;
}

/// A session's `archive` records the caller on the target's `Archived` entry, spelled
/// as the caller's messages are (docs/log-and-projection.md); the child that ends with
/// it records none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn archive_records_the_calling_session() {
    use crate::log::Payload;

    let fixture = Fixture::new();
    let child = fixture.host.session("child").unwrap();
    let grandchild = fixture.host.session("grandchild").unwrap();
    for session in [&child, &grandchild] {
        session.append(1, Payload::user("hi")).unwrap();
    }
    Archive {
        agent: "child".into(),
    }
    .execute(fixture.control("parent"), RequestContext::default())
    .await
    .unwrap();
    wait_for("the cascade", || {
        fixture.host.session("grandchild").is_none()
    })
    .await;

    let archived = |session: &Arc<Session>| match session.snapshot().last().unwrap().payload {
        Payload::Archived { ref source } => source.clone(),
        ref other => panic!("the log ends in {other:?}"),
    };
    assert_eq!(archived(&child).as_deref(), Some("session[parent]"));
    assert_eq!(archived(&grandchild), None);

    fixture.host.swansong().shut_down().await;
}

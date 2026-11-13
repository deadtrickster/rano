//! The client against a scripted letibot: a peer on the other end of a socket pair that speaks the
//! frames in the order the package's "done when" names them — attach, prompt, stream, decision,
//! interrupt — with an unknown event in the middle of it to be skipped.
//!
//! Nothing here opens a real socket and nothing touches a daemon's store: the peer is a
//! `UnixStream::pair()` half and every line it hears is asserted as JSON.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use rano::letibot::{
    AnswerMode, CitationKind, Client, ContextAsk, ContextPos, ContextRange, ContextRef,
    DecisionRequest, EditorContext, Event, Frame, MarkOutcome, OpenBuffer, PROTOCOL_VERSION, Reply,
    Seat,
};
use serde_json::Value;

/// Long enough that a failure to answer is the client's, short enough that a hang is not a wait.
const PATIENCE: Duration = Duration::from_secs(5);

/// One end of a socket pair, scripted by the test: it hears a line, answers what the test says.
struct Peer {
    out: UnixStream,
    heard: BufReader<UnixStream>,
}

impl Peer {
    /// A client attached to a peer nothing else holds, which is the whole fixture.
    fn pair() -> (Client, Peer) {
        let (ours, theirs) = UnixStream::pair().expect("a socket pair");
        let client = Client::on(ours).expect("a client on our half");
        // A peer that blocks forever on a client that sent nothing is a hang, not a failure.
        theirs.set_read_timeout(Some(PATIENCE)).expect("a deadline");
        let peer = Peer {
            out: theirs.try_clone().expect("a second handle on their half"),
            heard: BufReader::new(theirs),
        };
        (client, peer)
    }
    /// The next frame the client sent, as JSON.
    fn hear(&mut self) -> Value {
        let mut line = String::new();
        self.heard
            .read_line(&mut line)
            .expect("a line from the client");
        serde_json::from_str(line.trim()).unwrap_or_else(|e| panic!("not JSON ({e}): {line}"))
    }

    /// One frame out, verbatim: these are the bytes a daemon parses, so the test writes them as
    /// they would appear on the wire. The fixtures are wrapped for reading, so the newlines in
    /// them are JSON's whitespace and are flattened to keep one frame on one line.
    fn say(&mut self, frame: &str) {
        let line = frame.replace('\n', " ");
        writeln!(self.out, "{}", line.trim()).expect("a line to the client");
        self.out.flush().expect("flushed");
    }

    /// The `Hello` an attach is answered with, and every session the daemon holds.
    fn hello(&mut self) {
        self.say(
            r#"{"frame":"hello","protocol_version":70,"session_id":"s-1","head_id":"h-1",
                "dropped":0,"resumed_from":null,"snapshot":{"session_id":"s-1","seq":0,
                "dropped":0,"items_dropped":0,"items":[],"decisions":[]},
                "wiring":{"model":"m"},"scrubbed":{"decisions":0,"progress":0},
                "sessions":[{"session_id":"s-1","title":"the one","created_ms":9,
                "status":{},"wiring":{},"live":true,"stored_items":0}]}"#,
        );
    }
}

/// The done-when's first half: the client attaches, and what left is the editor seat letibot
/// documents — `kind: "editor"`, a version, and caps that say it can answer.
#[test]
fn attaching_announces_an_editor_head_and_reads_the_hello() {
    let (mut client, mut peer) = Peer::pair();
    client
        .attach("s-1", 0, &Seat::default())
        .expect("an attach is written");

    let attach = peer.hear();
    assert_eq!(attach["frame"], "attach");
    assert_eq!(attach["kind"], "editor");
    assert_eq!(attach["session_id"], "s-1");
    assert_eq!(attach["since_seq"], 0);
    assert_eq!(attach["protocol_version"], PROTOCOL_VERSION);
    assert_eq!(attach["caps"]["can_decide"].as_bool(), Some(true));
    assert_eq!(attach["caps"]["features"][0], "editor_v1");

    peer.hello();
    let Some(Frame::Hello(hello)) = client.frame(PATIENCE) else {
        panic!("a hello follows an attach");
    };
    assert_eq!(hello.head_id, "h-1");
    assert_eq!(hello.session_id, "s-1");
    assert_eq!(hello.protocol_version, 70);
    assert_eq!(hello.sessions[0].title, "the one");
}

/// The two things this client was moved to protocol 72 to speak: what a prompt carries beside its
/// words (71), and the kind of thing a citation is (72).
#[test]
fn a_prompt_carries_the_context_and_a_citation_carries_its_kind() {
    let (mut client, mut peer) = Peer::pair();
    client.attach("s-1", 0, &Seat::default()).unwrap();
    let _ = peer.hear();
    peer.hello();
    assert!(client.frame(PATIENCE).is_some());

    let context = EditorContext {
        origin: "rano".to_string(),
        refs: vec![ContextRef {
            file: "/w/article.md".to_string(),
            range: Some(ContextRange {
                start: ContextPos {
                    line: 41,
                    column: 1,
                },
                end: ContextPos {
                    line: 47,
                    column: 9,
                },
            }),
            cursor: Some(ContextPos {
                line: 41,
                column: 1,
            }),
            text: "the first transatlantic cable".to_string(),
            unsaved: true,
            lang: "markdown".to_string(),
            title: String::new(),
        }],
        open: vec![OpenBuffer {
            file: "/w/notes.md".to_string(),
            unsaved: false,
            visible: true,
        }],
        ask: Some(ContextAsk {
            preset: "fact-check".to_string(),
            answer: AnswerMode::Notes,
        }),
        outcomes: vec![MarkOutcome {
            label: "article.md:41-47".to_string(),
            outcome: "accepted".to_string(),
        }],
    };
    client
        .prompt_with_context("check this", &[], Some(&context))
        .expect("a prompt is written");
    let prompt = peer.hear();
    let sent = &prompt["context"];
    assert_eq!(sent["origin"], "rano");
    assert_eq!(sent["refs"][0]["file"], "/w/article.md");
    assert_eq!(sent["refs"][0]["range"]["start"]["line"], 41);
    assert_eq!(sent["refs"][0]["range"]["end"]["column"], 9);
    assert_eq!(sent["refs"][0]["text"], "the first transatlantic cable");
    assert_eq!(sent["refs"][0]["unsaved"], true);
    assert_eq!(sent["refs"][0]["lang"], "markdown");
    assert_eq!(sent["open"][0]["file"], "/w/notes.md");
    assert_eq!(sent["ask"]["preset"], "fact-check");
    assert_eq!(sent["ask"]["answer"], "notes");
    assert_eq!(sent["outcomes"][0]["label"], "article.md:41-47");
    assert_eq!(sent["outcomes"][0]["outcome"], "accepted");

    peer.say(
        r#"{"frame":"event","seq":1,"event":"citations","item_id":"it-1",
            "citations":[{"label":"article.md:41-47","path":"article.md","start":41,
            "end":47,"lines":["a","b"],"error":null,"kind":"note",
            "body":"warn: no source for this"}]}"#,
    );
    let Some(Frame::Event {
        event: Event::Citations { item_id, citations },
        ..
    }) = client.frame(PATIENCE)
    else {
        panic!("a citations event");
    };
    assert_eq!(item_id, "it-1");
    assert_eq!(citations[0].kind, CitationKind::Note);
    assert_eq!(citations[0].body, "warn: no source for this");
    assert_eq!(citations[0].lines, vec!["a".to_string(), "b".to_string()]);

    // A daemon that never heard of the field still reads as a citation of the bare kind.
    peer.say(
        r#"{"frame":"event","seq":2,"event":"citations","item_id":"it-2",
            "citations":[{"label":"a.md:1-1","path":"a.md","start":1,"end":1,
            "lines":[]}]}"#,
    );
    let Some(Frame::Event {
        event: Event::Citations { citations, .. },
        ..
    }) = client.frame(PATIENCE)
    else {
        panic!("a citations event");
    };
    assert_eq!(citations[0].kind, CitationKind::Cite, "70 and 71 mean cite");
    assert!(citations[0].body.is_empty());

    // And a finished call's edits, both sides of each, which are the editor's `edit` marks.
    peer.say(
        r#"{"frame":"event","seq":3,"event":"tool_finished","turn_id":"t-1",
            "call_id":"c-1","outcome":{"outcome":"ok"},"edits":[{"path":"article.md",
            "created":false,"before_start":41,"after_start":41,"before_lines":50,
            "after_lines":50,"truncated":false,"before":"old line","after":"new line"}]}"#,
    );
    let Some(Frame::Event {
        event: Event::ToolFinished { edits, .. },
        ..
    }) = client.frame(PATIENCE)
    else {
        panic!("a tool_finished event");
    };
    assert_eq!(edits.len(), 1);
    assert_eq!(edits[0].path, "article.md");
    assert_eq!((edits[0].before_start, edits[0].after_start), (41, 41));
    assert_eq!(edits[0].before, "old line");
    assert_eq!(edits[0].after, "new line");

    // One object rather than a list is what a row written before the list carries.
    peer.say(
        r#"{"frame":"event","seq":4,"event":"tool_finished","turn_id":"t-1",
            "call_id":"c-2","outcome":"ok","edits":{"path":"b.md","before":"x",
            "after":"y","before_start":1,"after_start":1}}"#,
    );
    let Some(Frame::Event {
        event: Event::ToolFinished { edits, .. },
        ..
    }) = client.frame(PATIENCE)
    else {
        panic!("a tool_finished event");
    };
    assert_eq!(edits.len(), 1, "one object read as a one-item list");
    assert_eq!(edits[0].path, "b.md");
}

/// A prompt goes out with the seq this head has consumed, and the reply is folded out of the
/// deltas in the order they arrived — thinking into its own channel, not into the prose.
#[test]
fn a_prompt_folds_its_reply_out_of_the_deltas() {
    let (mut client, mut peer) = Peer::pair();
    client.attach("s-1", 0, &Seat::default()).unwrap();
    let _ = peer.hear();
    peer.hello();
    assert!(client.frame(PATIENCE).is_some());

    let id = client
        .prompt("why is it slow?")
        .expect("a prompt is written");
    let prompt = peer.hear();
    assert_eq!(prompt["frame"], "prompt");
    assert_eq!(prompt["client_request_id"], id.as_str());
    assert_eq!(prompt["text"], "why is it slow?");
    assert_eq!(prompt["expected_seq"], 0, "nothing has been seen yet");
    assert_eq!(prompt["attachments"], serde_json::json!([]));
    assert!(
        prompt.get("context").is_none(),
        "words with nothing pointed at send no context"
    );

    peer.say(
        r#"{"frame":"accepted","client_request_id":"rano-1","seq":1,
            "note":"queued as a user item"}"#,
    );
    peer.say(r#"{"frame":"event","seq":2,"event":"turn_started","turn_id":"t-1","model":"m"}"#);
    peer.say(
        r#"{"frame":"event","seq":3,"event":"delta","turn_id":"t-1","target":"reasoning",
            "text":"weighing "}"#,
    );
    peer.say(
        r#"{"frame":"event","seq":4,"event":"delta","turn_id":"t-1","target":"text",
            "text":"Because the "}"#,
    );
    peer.say(
        r#"{"frame":"event","seq":5,"event":"delta","turn_id":"t-1","target":"text",
            "text":"cache is cold."}"#,
    );
    peer.say(
        r#"{"frame":"event","seq":6,"event":"tool_started","turn_id":"t-1","call_id":"c-1",
            "name":"bash","access":"exec"}"#,
    );
    peer.say(
        r#"{"frame":"event","seq":7,"event":"tool_finished","turn_id":"t-1","call_id":"c-1",
            "outcome":{"outcome":"ok"},"payload_digest":"d","inline_bytes":0,"full_bytes":0,
            "spill":null,"repairs":0}"#,
    );
    peer.say(
        r#"{"frame":"event","seq":8,"event":"turn_finished","turn_id":"t-1",
            "finish_reason":"eos","usage":{},"timings":{},"ends_turn":true}"#,
    );

    let mut reply = Reply::default();
    let mut seen = 0;
    let mut ended = false;
    while let Some(frame) = client.frame(PATIENCE) {
        match &frame {
            Frame::Accepted { note, .. } => assert_eq!(note, "queued as a user item"),
            Frame::Event {
                event: Event::ToolStarted { name, access, .. },
                ..
            } => {
                assert_eq!((name.as_str(), access.as_str()), ("bash", "exec"));
            }
            Frame::Event {
                event: Event::ToolFinished { outcome, .. },
                ..
            } => assert_eq!(outcome, "ok"),
            Frame::Event {
                event: Event::TurnFinished { ends_turn, .. },
                ..
            } => {
                ended = *ends_turn;
            }
            _ => {}
        }
        reply.fold(&frame);
        seen += 1;
        if ended && seen >= 7 {
            break;
        }
    }
    assert_eq!(reply.text, "Because the cache is cold.");
    assert_eq!(reply.thinking, "weighing ");
    assert!(ended, "the turn said it ended");
    assert_eq!(client.seq(), 8, "the seq the deltas reached");

    // And the next command states the seq this head has now seen, so the daemon can tell a head
    // that kept up from one whose screen moved on without it.
    let _ = client.interrupt("a second thought");
    assert_eq!(peer.hear()["expected_seq"], 8);
}

/// A card is answered by the id its ladder offered, and the answer names the card's `req_id`.
#[test]
fn a_decision_is_answered_by_its_option_id() {
    let (mut client, mut peer) = Peer::pair();
    client.attach("s-1", 0, &Seat::default()).unwrap();
    let _ = peer.hear();
    peer.hello();
    assert!(client.frame(PATIENCE).is_some());

    peer.say(
        r#"{"frame":"event","seq":1,"event":"decision_requested","req_id":"r-1",
            "kind":"permission","call_id":"c-1","access":"exec",
            "summary":"bash would run `rm -rf build`","target":"rm -rf build","detail":"",
            "options":[{"option_id":"allow_once","label":"once","kind":"allow_once"},
                       {"option_id":"reject_once","label":"no","kind":"reject_once"}],
            "options_note":"","choices":[],"because":"","deadline":null,"on_timeout":"deny"}"#,
    );

    let Some(Frame::Event {
        event:
            Event::DecisionRequested(DecisionRequest {
                req_id,
                options,
                kind,
                ..
            }),
        seq,
    }) = client.frame(PATIENCE)
    else {
        panic!("a card arrives as a card");
    };
    assert_eq!(seq, 1);
    assert_eq!(kind, "permission");
    assert_eq!(options.len(), 2);

    let answer = client
        .answer(&req_id, &options[0].option_id)
        .expect("an answer is written");
    let said = peer.hear();
    assert_eq!(said["frame"], "answer");
    assert_eq!(said["client_request_id"], answer.as_str());
    assert_eq!(said["req_id"], "r-1");
    assert_eq!(said["option_id"], "allow_once");
    assert!(said.get("note").is_none(), "nothing was said with it");

    peer.say(r#"{"frame":"accepted","client_request_id":"rano-2","seq":2,"note":"answered"}"#);
    assert!(matches!(
        client.frame(PATIENCE),
        Some(Frame::Accepted { .. })
    ));
}

/// A question is a different door from a permission, and an abstention is an answer.
#[test]
fn a_question_is_answered_through_its_own_frame() {
    let (mut client, mut peer) = Peer::pair();
    client.attach("s-1", 0, &Seat::default()).unwrap();
    let _ = peer.hear();
    peer.hello();
    assert!(client.frame(PATIENCE).is_some());

    let answer = rano::letibot::Answer {
        free: Some("the second one".to_string()),
        ..rano::letibot::Answer::default()
    };
    client.answer_question("q-1", &answer).expect("written");
    let said = peer.hear();
    assert_eq!(said["frame"], "answer_question");
    assert_eq!(said["req_id"], "q-1");
    assert_eq!(said["answer"]["free"], "the second one");
}

/// An interrupting head says why, because the reason is what the other heads are told.
#[test]
fn an_interrupt_carries_the_reason() {
    let (mut client, mut peer) = Peer::pair();
    client.attach("s-1", 0, &Seat::default()).unwrap();
    let _ = peer.hear();
    peer.hello();
    assert!(client.frame(PATIENCE).is_some());

    let id = client
        .interrupt("esc esc")
        .expect("an interrupt is written");
    let said = peer.hear();
    assert_eq!(said["frame"], "interrupt");
    assert_eq!(said["client_request_id"], id.as_str());
    assert_eq!(said["reason"], "esc esc");
    assert_eq!(said["expected_seq"], 0);
}

/// The package's tolerance, end to end: an event this build has never heard of is skipped and
/// counted, the stream carries on, and the delta after it still lands in the reply.
#[test]
fn an_unknown_event_is_skipped_and_the_stream_carries_on() {
    let (mut client, mut peer) = Peer::pair();
    client.attach("s-1", 0, &Seat::default()).unwrap();
    let _ = peer.hear();
    peer.hello();
    assert!(client.frame(PATIENCE).is_some());

    peer.say(r#"{"frame":"event","seq":1,"event":"explain","turn_id":"t-1","plan":{}}"#);
    let skipped = client.frame(PATIENCE).expect("the unknown event arrives");
    assert_eq!(
        skipped,
        Frame::Event {
            seq: 1,
            event: Event::Unknown {
                tag: "explain".to_string()
            },
        }
    );
    assert!(skipped.is_unknown());
    assert_eq!(client.skipped(), 1);

    // A frame whose tag is new, a line that is not JSON at all, and a field nothing here reads:
    // all three, and then a delta that must still be folded.
    peer.say(r#"{"frame":"telemetry","rows":[{"k":1}]}"#);
    peer.say("this is not a frame");
    peer.say(
        r#"{"frame":"event","seq":2,"event":"delta","turn_id":"t-1","target":"text",
            "text":"still here","future_field":{"deep":true}}"#,
    );
    let mut reply = Reply::default();
    loop {
        let Some(frame) = client.frame(PATIENCE) else {
            panic!("the stream ended at the unknown frames");
        };
        let last = reply.fold(&frame);
        if last {
            break;
        }
    }
    assert_eq!(reply.text, "still here");
    assert_eq!(client.skipped(), 3, "the event, the frame, the junk line");
    assert_eq!(client.seq(), 2);
}

/// A resync is not an error: the panel is told where to start again from, and the seq it had is
/// replaced by the snapshot's rather than kept.
#[test]
fn a_resync_names_the_seq_to_carry_on_from() {
    let (mut client, mut peer) = Peer::pair();
    client.attach("s-1", 0, &Seat::default()).unwrap();
    let _ = peer.hear();
    peer.hello();
    assert!(client.frame(PATIENCE).is_some());

    peer.say(
        r#"{"frame":"resync","reason":"queue overflowed","dropped":12,
            "snapshot":{"session_id":"s-1","seq":40,"dropped":12,"items_dropped":0,
            "items":[],"decisions":[]},"scrubbed":{}}"#,
    );
    let Some(Frame::Resync {
        reason,
        dropped,
        seq,
    }) = client.frame(PATIENCE)
    else {
        panic!("a resync arrives as one");
    };
    assert_eq!(
        (reason.as_str(), dropped, seq),
        ("queue overflowed", 12, 40)
    );
    assert_eq!(client.seq(), 40);

    // The mark the design pairs with it: a head that resynced asks for a fresh snapshot.
    client.resync().expect("a resync is written");
    assert_eq!(peer.hear()["frame"], "resync");
}

/// A daemon that says goodbye ends the stream, and the end is a value rather than a panic or a
/// silent hang: the editor is never blocked by a daemon going away.
#[test]
fn a_bye_ends_the_stream_cleanly() {
    let (mut client, mut peer) = Peer::pair();
    client.attach("s-1", 0, &Seat::default()).unwrap();
    let _ = peer.hear();

    peer.say(r#"{"frame":"bye","reason":"daemon shutting down"}"#);
    let Some(Frame::Bye { reason }) = client.frame(PATIENCE) else {
        panic!("a bye arrives as a bye");
    };
    assert_eq!(reason, "daemon shutting down");

    // The peer going away is a detach: the next read answers now, not when a deadline passes.
    drop(peer);
    let started = std::time::Instant::now();
    assert!(
        client.frame(PATIENCE).is_none(),
        "a closed socket ends the stream"
    );
    assert!(started.elapsed() < Duration::from_secs(1));
}

/// The read mark is what tells the daemon how much of the batch was drawn and how much was
/// filtered, which are two different facts.
#[test]
fn the_read_mark_says_what_was_drawn() {
    let (mut client, mut peer) = Peer::pair();
    client.attach("s-1", 0, &Seat::default()).unwrap();
    let _ = peer.hear();

    peer.say(r#"{"frame":"event","seq":3,"event":"turn_started","turn_id":"t","model":"m"}"#);
    assert!(client.frame(PATIENCE).is_some());
    client.ack(1, 0).expect("an ack is written");
    let said = peer.hear();
    assert_eq!(said["frame"], "ack");
    assert_eq!(said["seq"], 3);
    assert_eq!(said["rendered"], 1);
    assert_eq!(said["filtered"], 0);
}

/// The session picker's two frames: a list, and a new session that does not move this head.
#[test]
fn the_session_list_and_a_new_session_are_the_documented_frames() {
    let (mut client, mut peer) = Peer::pair();
    client.attach("s-1", 0, &Seat::default()).unwrap();
    let _ = peer.hear();
    peer.hello();
    assert!(client.frame(PATIENCE).is_some());

    client.list_sessions().expect("a list is written");
    assert_eq!(peer.hear()["frame"], "list_sessions");

    peer.say(
        r#"{"frame":"sessions","current":"s-1","created":null,"sessions":[
            {"session_id":"s-1","title":"the one","created_ms":9,"status":{},"wiring":{},
             "live":true,"stored_items":0},
            {"session_id":"s-0","title":"","created_ms":1,"status":{},"wiring":{},
             "live":false,"stored_items":12}]}"#,
    );
    let Some(Frame::Sessions {
        current,
        created,
        sessions,
    }) = client.frame(PATIENCE)
    else {
        panic!("a sessions frame arrives as one");
    };
    assert_eq!(current, "s-1");
    assert_eq!(created, None);
    assert_eq!(sessions.len(), 2);
    assert_eq!(sessions[0].title, "the one");
    assert_eq!(sessions[1].id, "s-0");
    assert!(!sessions[1].live, "a session only in the store is not live");

    let id = client.new_session("a name", "/w").expect("a new session");
    let said = peer.hear();
    assert_eq!(said["frame"], "new_session");
    assert_eq!(said["client_request_id"], id.as_str());
    assert_eq!(said["title"], "a name");
    assert_eq!(said["workspace"], "/w");

    client.switch("s-0", 12).expect("a switch");
    let said = peer.hear();
    assert_eq!(said["frame"], "switch");
    assert_eq!(said["session_id"], "s-0");
    assert_eq!(said["since_seq"], 12);
}

/// History is a page of rows by ordinal, and the rows a panel does not know are still delivered.
#[test]
fn a_page_of_rows_is_asked_for_by_ordinal() {
    let (mut client, mut peer) = Peer::pair();
    client.attach("s-1", 0, &Seat::default()).unwrap();
    let _ = peer.hear();

    client.fetch_rows("s-1", 0, 40).expect("a fetch");
    let said = peer.hear();
    assert_eq!(said["frame"], "fetch_rows");
    assert_eq!(said["from"], 0);
    assert_eq!(said["count"], 40);

    peer.say(
        r#"{"frame":"rows_fetched","session_id":"s-1","from":0,"rows":[
            {"item_id":"i-1","kind":"user","ledger_head":"l-1","item":{"kind":"text"}},
            {"item_id":"i-2","kind":"something_new","ledger_head":"l-2"}]}"#,
    );
    let Some(Frame::RowsFetched { from, rows }) = client.frame(PATIENCE) else {
        panic!("rows arrive as rows");
    };
    assert_eq!(from, 0);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1]["item_id"], "i-2");
}

/// The done-when in one connection and in a session's own order: attach, prompt, deltas into text,
/// a card answered, an interrupt — with an unknown event skipped between two deltas rather than
/// ending anything, which is the tolerance the whole module exists for.
#[test]
fn the_whole_subset_on_one_connection() {
    let (mut client, mut peer) = Peer::pair();
    client
        .attach("s-1", 0, &Seat::default())
        .expect("an attach is written");
    assert_eq!(peer.hear()["frame"], "attach");
    peer.hello();
    assert!(matches!(client.frame(PATIENCE), Some(Frame::Hello(_))));

    client.prompt("why is it slow?").expect("a prompt");
    assert_eq!(peer.hear()["frame"], "prompt");

    peer.say(
        r#"{"frame":"event","seq":1,"event":"delta","turn_id":"t-1","target":"text",
            "text":"the answer "}"#,
    );
    peer.say(r#"{"frame":"event","seq":2,"event":"explain","turn_id":"t-1","plan":{}}"#);
    peer.say(
        r#"{"frame":"event","seq":3,"event":"delta","turn_id":"t-1","target":"text",
            "text":"is 42."}"#,
    );
    peer.say(
        r#"{"frame":"event","seq":4,"event":"decision_requested","req_id":"r-1",
            "kind":"permission","call_id":"c-1","access":"exec","summary":"run it",
            "target":"rm -rf build","detail":"","options":[
              {"option_id":"allow_once","label":"once","kind":"allow_once"}],
            "options_note":"","choices":[],"because":"","deadline":null,
            "on_timeout":"deny"}"#,
    );

    let mut reply = Reply::default();
    let card = loop {
        let frame = client
            .frame(PATIENCE)
            .expect("the stream carried on past the unknown event");
        reply.fold(&frame);
        if let Frame::Event {
            event: Event::DecisionRequested(card),
            ..
        } = frame
        {
            break card;
        }
    };
    assert_eq!(reply.text, "the answer is 42.");
    assert_eq!(client.skipped(), 1, "the event nothing here knows");
    assert_eq!(card.req_id, "r-1");

    client
        .answer(&card.req_id, &card.options[0].option_id)
        .expect("an answer is written");
    let said = peer.hear();
    assert_eq!(said["frame"], "answer");
    assert_eq!(said["option_id"], "allow_once");

    peer.say(r#"{"frame":"accepted","client_request_id":"rano-2","seq":5,"note":"answered"}"#);
    assert!(matches!(
        client.frame(PATIENCE),
        Some(Frame::Accepted { .. })
    ));

    client.interrupt("esc esc").expect("an interrupt");
    let said = peer.hear();
    assert_eq!(said["frame"], "interrupt");
    assert_eq!(said["reason"], "esc esc");
    assert_eq!(said["expected_seq"], 4, "the seq of the last event it saw");

    peer.say(
        r#"{"frame":"event","seq":5,"event":"turn_interrupted","turn_id":"t-1",
            "reason":"esc esc","partial_kept":true}"#,
    );
    let Some(Frame::Event {
        event: Event::TurnInterrupted { reason, .. },
        seq,
    }) = client.frame(PATIENCE)
    else {
        panic!("an interrupt is answered with an ending");
    };
    assert_eq!((reason.as_str(), seq), ("esc esc", 5));
}

/// A `$LETIBOT_SOCKET` names the socket without asking anything, which is the arm a test can drive
/// with no launcher and no daemon at all.
#[test]
fn a_named_socket_is_the_one_the_panel_connects_to() {
    let named = std::path::Path::new("/run/letibot/0123456789ab.sock");
    assert_eq!(
        rano::letibot::resolve(
            std::path::Path::new("/w"),
            Some("/run/letibot/0123456789ab.sock")
        ),
        Ok(named.to_path_buf())
    );
}

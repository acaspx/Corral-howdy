//! The walking skeleton's core half (SK-6a): TP-SKEL-03..05 (`docs/04-test-plan.md`, J0).
//!
//! - **Agent text** (security model T20): what an agent writes is cleaned before any surface
//!   draws it: control and bidi characters stripped, length capped, the cut flagged.
//! - **Run cards** (TP-SKEL-04, 05): what the hover card and the result card show, projected from
//!   the log. Elapsed time and spend come from here, never from Swift's own counting.
//! - **Controls** (TP-SKEL-03, 05): what a click on a drop does. Deploy, Stop, Send back and
//!   Retry are decided here (AGENTS.md rule 3).

use corral_core::agent_text::{ANSWER_MAX_CHARS, AgentText, PROMPT_MAX_CHARS};
use corral_core::clock::{Clock, ManualClock, SystemClock};
use corral_core::controls::{ControlError, CorralControls, MAX_PROMPT_BYTES};
use corral_core::corral_view::ViewError;
use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId, SpanId};
use corral_core::lifecycle::{Lifecycle, Phase, next};
use corral_core::run_card::{CardAction, CardStatus, RunCards};
use corral_core::supervisor::{
    CancelToken, EventLog, EventSink, LogError, RunDriver, RunFuture, RunRequest, Supervisor,
    SupervisorConfig, SupervisorError,
};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// ---- Agent text (T20) -------------------------------------------------------------------------

#[test]
fn tp_skel_05_agent_text_strips_control_and_bidi_characters() {
    let raw = "Paris\u{0007} is\u{202E}lanigiro\u{202C} the\u{2066} capital\u{2069}\u{200B}.\u{FEFF}\u{061C}\u{200F}\u{009B}";
    let text = AgentText::new(raw, ANSWER_MAX_CHARS);
    assert_eq!(text.as_str(), "Paris islanigiro the capital.");
    assert!(!text.is_truncated());
}

#[test]
fn tp_skel_05_agent_text_keeps_lines_tabs_as_spaces_and_emoji() {
    let raw = "  Line one\r\nLine\ttwo\n\n\n\n👩\u{200D}💻 done  \n";
    let text = AgentText::new(raw, ANSWER_MAX_CHARS);
    assert_eq!(text.as_str(), "Line one\nLine two\n\n👩\u{200D}💻 done");
}

#[test]
fn tp_skel_05_agent_text_is_capped_in_characters_and_says_so() {
    let raw = "é".repeat(PROMPT_MAX_CHARS + 10);
    let text = AgentText::new(&raw, PROMPT_MAX_CHARS);
    assert_eq!(text.as_str().chars().count(), PROMPT_MAX_CHARS);
    assert!(text.is_truncated());
    let exact = AgentText::new(&"a".repeat(PROMPT_MAX_CHARS), PROMPT_MAX_CHARS);
    assert!(!exact.is_truncated());
}

#[test]
fn tp_skel_05_the_caps_are_what_the_cards_promise() {
    assert_eq!(ANSWER_MAX_CHARS, 4_000);
    assert_eq!(PROMPT_MAX_CHARS, 280);
}

// ---- Run cards (TP-SKEL-04, 05) ------------------------------------------------------------

fn ev(run: &str, ts: u64, body: EventBody) -> Event {
    Event {
        run_id: RunId::from(run),
        seq: 1,
        ts,
        span_id: SpanId::from("span"),
        parent_span_id: None,
        body,
    }
}

fn queued(run: &str, agent: &str, prompt: &str, ts: u64) -> Event {
    ev(
        run,
        ts,
        EventBody::RunQueued(RunQueued {
            agent_id: AgentId::from(agent),
            session_id: SessionId::from("s1"),
            prompt: prompt.into(),
        }),
    )
}

fn started(run: &str, ts: u64) -> Event {
    ev(
        run,
        ts,
        EventBody::RunStarted(RunStarted {
            adapter: "xai".into(),
        }),
    )
}

fn request(run: &str, ts: u64) -> Event {
    ev(
        run,
        ts,
        EventBody::ModelRequest(ModelRequest {
            provider: "xai".into(),
            model: "grok-test".into(),
        }),
    )
}

fn delta(run: &str, ts: u64, text: &str) -> Event {
    ev(
        run,
        ts,
        EventBody::ModelDelta(ModelDelta { text: text.into() }),
    )
}

fn response(run: &str, ts: u64, text: &str) -> Event {
    ev(
        run,
        ts,
        EventBody::ModelResponse(ModelResponse {
            text: text.into(),
            finish_reason: Some("completed".into()),
        }),
    )
}

fn usage(run: &str, ts: u64, cost: u64) -> Event {
    ev(
        run,
        ts,
        EventBody::UsageReported(UsageReported {
            input_tokens: 10,
            output_tokens: 20,
            cost_micro_usd: cost,
        }),
    )
}

fn completed(run: &str, ts: u64) -> Event {
    ev(
        run,
        ts,
        EventBody::RunCompleted(RunCompleted { summary: None }),
    )
}

fn failed(run: &str, ts: u64, message: &str) -> Event {
    ev(
        run,
        ts,
        EventBody::RunFailed(RunFailed {
            reason: FailureReason::Provider,
            message: message.into(),
        }),
    )
}

fn cancelled(run: &str, ts: u64) -> Event {
    ev(
        run,
        ts,
        EventBody::RunCancelled(RunCancelled {
            by: CancelledBy::User,
        }),
    )
}

fn closed(run: &str, ts: u64) -> Event {
    ev(run, ts, EventBody::SessionClosed(SessionClosed {}))
}

fn grok() -> AgentId {
    AgentId::from("grok")
}

fn feed(cards: &mut RunCards, events: &[Event]) {
    for e in events {
        cards.observe(e).expect("the lifecycle accepts the event");
    }
}

#[test]
fn tp_skel_04_a_working_card_counts_time_from_run_started_and_spend_from_usage() {
    let mut cards = RunCards::default();
    feed(
        &mut cards,
        &[
            queued("r1", "grok", "What is the capital of France?", 1_000),
            started("r1", 1_500),
            request("r1", 1_600),
            usage("r1", 2_000, 306),
            usage("r1", 2_100, 94),
        ],
    );
    let card = cards.card(&grok(), 4_500).expect("a card while working");
    assert_eq!(card.run, RunId::from("r1"));
    assert_eq!(card.status, CardStatus::Working);
    assert_eq!(card.elapsed_ms, 3_000, "from run.started to now");
    assert_eq!(card.spend_micro_usd, 400);
    assert_eq!(card.spend, "$0.0004");
    assert_eq!(card.prompt.as_str(), "What is the capital of France?");
    assert_eq!(card.answer, None);
    assert_eq!(card.actions, [CardAction::Stop]);
    // A second later, the same card has moved on by a second.
    assert_eq!(
        cards.card(&grok(), 5_500).map(|c| c.elapsed_ms),
        Some(4_000)
    );
    assert!(
        cards.is_ticking(),
        "the hover card needs a tick while working"
    );
}

#[test]
fn tp_skel_04_a_queued_card_shows_no_time_yet() {
    let mut cards = RunCards::default();
    feed(&mut cards, &[queued("r1", "grok", "hi", 1_000)]);
    let card = cards.card(&grok(), 9_000).expect("card");
    assert_eq!(card.status, CardStatus::Queued);
    assert_eq!(card.elapsed_ms, 0);
    assert_eq!(card.actions, [CardAction::Stop]);
}

#[test]
fn tp_skel_04_observe_reports_card_changes_but_not_streamed_text_or_time() {
    let mut cards = RunCards::default();
    assert!(
        cards
            .observe(&queued("r1", "grok", "hi", 1_000))
            .expect("ok")
    );
    assert!(
        cards.observe(&started("r1", 1_500)).expect("ok"),
        "queued -> working"
    );
    assert!(
        !cards.observe(&request("r1", 1_600)).expect("ok"),
        "nothing shown changed"
    );
    assert!(
        !cards.observe(&delta("r1", 1_700, "Par")).expect("ok"),
        "streamed text isn't shown until the answer is in"
    );
    assert!(
        cards.observe(&usage("r1", 1_800, 306)).expect("ok"),
        "spend"
    );
    assert!(
        cards.observe(&response("r1", 1_900, "Paris.")).expect("ok"),
        "the answer"
    );
    assert!(cards.observe(&completed("r1", 2_000)).expect("ok"), "done");
}

#[test]
fn tp_skel_05_a_done_card_shows_the_cleaned_answer_and_stops_the_clock() {
    let mut cards = RunCards::default();
    feed(
        &mut cards,
        &[
            queued("r1", "grok", "Capital of France?", 1_000),
            started("r1", 1_500),
            request("r1", 1_600),
            delta("r1", 1_700, "Paris"),
            response("r1", 1_800, "Paris\u{202E} is the capital.\u{0007}"),
            usage("r1", 1_900, 306),
            completed("r1", 2_500),
        ],
    );
    let card = cards.card(&grok(), 60_000).expect("done card");
    assert_eq!(card.status, CardStatus::Done);
    assert_eq!(card.elapsed_ms, 1_000, "the clock stops at the end");
    let answer = card.answer.expect("the answer");
    assert_eq!(answer.as_str(), "Paris is the capital.");
    assert_eq!(card.actions, [CardAction::SendBack, CardAction::NewTask]);
    assert!(!cards.is_ticking(), "nothing left to tick");
}

#[test]
fn tp_skel_05_a_long_answer_is_capped() {
    let mut cards = RunCards::default();
    let long = "word ".repeat(2_000);
    feed(
        &mut cards,
        &[
            queued("r1", "grok", "Write a lot.", 1_000),
            started("r1", 1_000),
            request("r1", 1_000),
            response("r1", 1_000, &long),
            completed("r1", 2_000),
        ],
    );
    let answer = cards
        .card(&grok(), 2_000)
        .and_then(|c| c.answer)
        .expect("answer");
    assert!(answer.is_truncated());
    assert!(answer.as_str().chars().count() <= ANSWER_MAX_CHARS);
}

#[test]
fn tp_skel_05_a_summary_stands_in_when_no_response_text_was_logged() {
    let mut cards = RunCards::default();
    feed(
        &mut cards,
        &[
            queued("r1", "grok", "hi", 1_000),
            started("r1", 1_000),
            ev(
                "r1",
                2_000,
                EventBody::RunCompleted(RunCompleted {
                    summary: Some("All done.".into()),
                }),
            ),
        ],
    );
    let card = cards.card(&grok(), 2_000).expect("card");
    assert_eq!(
        card.answer.map(|a| a.as_str().to_owned()),
        Some("All done.".into())
    );
}

#[test]
fn tp_skel_05_a_failed_card_shows_why_and_offers_retry() {
    let mut cards = RunCards::default();
    feed(
        &mut cards,
        &[
            queued("r1", "grok", "hi", 1_000),
            started("r1", 1_000),
            failed("r1", 3_000, "Add your xAI key\u{202E}."),
        ],
    );
    let card = cards.card(&grok(), 9_000).expect("card");
    assert_eq!(card.status, CardStatus::Error);
    assert_eq!(
        card.error.map(|e| e.as_str().to_owned()),
        Some("Add your xAI key.".into())
    );
    assert_eq!(card.actions, [CardAction::Retry, CardAction::SendBack]);
    assert_eq!(card.elapsed_ms, 2_000);
}

#[test]
fn tp_skel_05_a_stopped_card_offers_send_back_and_a_new_task() {
    let mut cards = RunCards::default();
    feed(
        &mut cards,
        &[
            queued("r1", "grok", "hi", 1_000),
            started("r1", 1_000),
            cancelled("r1", 1_200),
        ],
    );
    let card = cards.card(&grok(), 9_000).expect("card");
    assert_eq!(card.status, CardStatus::Stopped);
    assert_eq!(card.actions, [CardAction::SendBack, CardAction::NewTask]);
}

#[test]
fn tp_skel_05_send_back_clears_the_card() {
    let mut cards = RunCards::default();
    feed(
        &mut cards,
        &[
            queued("r1", "grok", "hi", 1_000),
            started("r1", 1_000),
            completed("r1", 2_000),
        ],
    );
    assert!(cards.observe(&closed("r1", 3_000)).expect("ok"));
    assert_eq!(cards.card(&grok(), 3_000), None, "the drop is ready again");
}

#[test]
fn tp_skel_05_the_card_follows_the_agents_latest_run() {
    let mut cards = RunCards::default();
    feed(
        &mut cards,
        &[
            queued("r1", "grok", "first", 1_000),
            started("r1", 1_000),
            failed("r1", 2_000, "boom"),
            queued("r2", "grok", "second", 3_000),
            started("r2", 3_000),
        ],
    );
    let card = cards.card(&grok(), 4_000).expect("card");
    assert_eq!(card.run, RunId::from("r2"));
    assert_eq!(card.prompt.as_str(), "second");
    assert_eq!(card.status, CardStatus::Working);
    assert_eq!(card.error, None);
    // Late events for the older run don't touch the card.
    assert!(!cards.observe(&closed("r1", 5_000)).expect("ok"));
    assert_eq!(
        cards.card(&grok(), 5_000).map(|c| c.run),
        Some(RunId::from("r2"))
    );
}

#[test]
fn tp_skel_04_the_card_never_disagrees_with_the_lifecycle() {
    let mut cards = RunCards::default();
    assert!(matches!(
        cards.observe(&started("never-queued", 1_000)),
        Err(ViewError::UnknownRun(_))
    ));
    feed(&mut cards, &[queued("r1", "grok", "hi", 1_000)]);
    assert!(matches!(
        cards.observe(&completed("r1", 2_000)),
        Err(ViewError::Lifecycle(_))
    ));
    assert_eq!(
        cards.card(&grok(), 2_000).map(|c| c.status),
        Some(CardStatus::Queued),
        "a refused event changes nothing"
    );
}

// ---- Controls (TP-SKEL-03, 05) -------------------------------------------------------------

#[derive(Default)]
struct MemLog(Mutex<Vec<Event>>);

impl EventLog for MemLog {
    fn append(&self, event: &Event) -> Result<(), LogError> {
        self.0.lock().expect("log lock").push(event.clone());
        Ok(())
    }
}

impl MemLog {
    fn events(&self) -> Vec<Event> {
        self.0.lock().expect("log lock").clone()
    }

    fn run(&self, run: &RunId) -> Vec<Event> {
        self.events()
            .into_iter()
            .filter(|e| &e.run_id == run)
            .collect()
    }

    fn kinds(&self, run: &RunId) -> Vec<&'static str> {
        self.run(run).iter().map(|e| e.kind().as_str()).collect()
    }

    fn queued(&self, run: &RunId) -> RunQueued {
        let first = self.run(run).into_iter().next().expect("events");
        let EventBody::RunQueued(q) = first.body else {
            panic!("the first event must be run.queued");
        };
        q
    }

    fn assert_legal(&self) {
        let mut runs: Vec<RunId> = self.events().into_iter().map(|e| e.run_id).collect();
        runs.dedup();
        for run in runs {
            let mut state = Lifecycle::idle();
            for e in self.run(&run) {
                state = next(&state, &e).unwrap_or_else(|err| panic!("illegal event: {err}"));
            }
        }
    }
}

/// What the scripted agent does with each prompt.
#[derive(Clone, Copy)]
enum Act {
    Answer,
    Fail,
    Hang,
}

struct Script(fn(&str) -> Act);

impl RunDriver for Script {
    fn run(&self, request: RunRequest, sink: EventSink, cancel: CancelToken) -> RunFuture {
        let act = (self.0)(&request.prompt);
        Box::pin(async move {
            match act {
                Act::Answer => {
                    sink.emit(EventBody::RunCompleted(RunCompleted {
                        summary: Some("done".into()),
                    }))
                    .expect("completed");
                }
                Act::Fail => {
                    sink.emit(EventBody::RunFailed(RunFailed {
                        reason: FailureReason::Provider,
                        message: "xAI is unavailable.".into(),
                    }))
                    .expect("failed");
                }
                Act::Hang => cancel.cancelled().await,
            }
        })
    }
}

fn by_prompt(prompt: &str) -> Act {
    if prompt.contains("fail") {
        Act::Fail
    } else if prompt.contains("hang") {
        Act::Hang
    } else {
        Act::Answer
    }
}

fn counter() -> Arc<dyn Fn() -> String + Send + Sync> {
    let n = Arc::new(AtomicU64::new(0));
    Arc::new(move || format!("{}", n.fetch_add(1, Ordering::SeqCst) + 1))
}

fn controls() -> (CorralControls, Supervisor, Arc<MemLog>) {
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        Arc::new(Script(by_prompt)),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    let controls = CorralControls::new(sup.clone(), [grok()], counter());
    (controls, sup, log)
}

async fn settle(sup: &Supervisor) {
    tokio::time::timeout(Duration::from_secs(30), sup.wait_idle())
        .await
        .expect("runs end");
}

#[tokio::test(start_paused = true)]
async fn tp_skel_03_deploying_from_a_drop_queues_then_starts_the_run() {
    let (controls, sup, log) = controls();
    let run = controls
        .deploy(&grok(), "  What is the capital of France?  ")
        .expect("deployed");
    settle(&sup).await;
    assert_eq!(
        log.kinds(&run)[..2],
        ["run.queued", "run.started"],
        "queued, then started"
    );
    let q = log.queued(&run);
    assert_eq!(q.agent_id, grok());
    assert_eq!(q.prompt, "What is the capital of France?", "trimmed");
    log.assert_legal();
}

#[tokio::test(start_paused = true)]
async fn tp_skel_03_a_deploy_is_refused_for_bad_input_and_nothing_is_logged() {
    let (controls, _sup, log) = controls();
    assert_eq!(
        controls.deploy(&AgentId::from("stranger"), "hi"),
        Err(ControlError::UnknownAgent(AgentId::from("stranger")))
    );
    assert_eq!(
        controls.deploy(&grok(), " \n\t "),
        Err(ControlError::EmptyPrompt)
    );
    assert_eq!(
        controls.deploy(&grok(), &"a".repeat(MAX_PROMPT_BYTES + 1)),
        Err(ControlError::PromptTooLong {
            max_bytes: MAX_PROMPT_BYTES
        })
    );
    assert!(log.events().is_empty());
}

#[tokio::test(start_paused = true)]
async fn tp_skel_03_a_working_agent_takes_no_second_task() {
    let (controls, _sup, log) = controls();
    controls.deploy(&grok(), "hang on").expect("first");
    assert_eq!(
        controls.deploy(&grok(), "another"),
        Err(ControlError::Busy(grok()))
    );
    assert_eq!(
        log.events().len(),
        2,
        "only the first run's queued and started"
    );
}

#[tokio::test(start_paused = true)]
async fn tp_skel_05_send_back_closes_the_session_and_the_next_deploy_starts_a_new_one() {
    let (controls, sup, log) = controls();
    let first = controls.deploy(&grok(), "hello").expect("deployed");
    settle(&sup).await;
    controls.send_back(&grok()).expect("sent back");
    assert_eq!(
        log.kinds(&first),
        [
            "run.queued",
            "run.started",
            "run.completed",
            "session.closed"
        ]
    );
    assert_eq!(
        controls.send_back(&grok()),
        Err(ControlError::NothingToSendBack(grok())),
        "already back in the corral"
    );
    let second = controls.deploy(&grok(), "again").expect("deployed");
    settle(&sup).await;
    assert_ne!(
        log.queued(&first).session_id,
        log.queued(&second).session_id,
        "a fresh session"
    );
    log.assert_legal();
}

#[tokio::test(start_paused = true)]
async fn tp_skel_05_a_new_task_on_a_done_agent_stays_in_its_session() {
    let (controls, sup, log) = controls();
    let first = controls.deploy(&grok(), "hello").expect("deployed");
    settle(&sup).await;
    let second = controls.deploy(&grok(), "and then?").expect("follow-up");
    settle(&sup).await;
    assert_eq!(
        log.queued(&first).session_id,
        log.queued(&second).session_id,
        "prompt again keeps the session"
    );
    assert_eq!(
        log.kinds(&first).last(),
        Some(&"run.completed"),
        "not closed"
    );
}

#[tokio::test(start_paused = true)]
async fn tp_skel_05_retry_reruns_the_failed_prompt_in_the_same_session() {
    let (controls, sup, log) = controls();
    let first = controls.deploy(&grok(), "please fail").expect("deployed");
    settle(&sup).await;
    assert_eq!(log.kinds(&first).last(), Some(&"run.failed"));
    let retry = controls.retry(&grok()).expect("retried");
    settle(&sup).await;
    assert_ne!(retry, first, "a new run");
    let (a, b) = (log.queued(&first), log.queued(&retry));
    assert_eq!(a.prompt, b.prompt);
    assert_eq!(a.session_id, b.session_id);
    log.assert_legal();
}

#[tokio::test(start_paused = true)]
async fn tp_skel_05_retry_is_only_for_a_failed_run() {
    let (controls, sup, _log) = controls();
    assert_eq!(
        controls.retry(&grok()),
        Err(ControlError::NothingToRetry(grok()))
    );
    controls.deploy(&grok(), "hello").expect("deployed");
    settle(&sup).await;
    assert_eq!(
        controls.retry(&grok()),
        Err(ControlError::NothingToRetry(grok())),
        "it didn't fail"
    );
}

#[tokio::test(start_paused = true)]
async fn tp_skel_05_stop_cancels_the_working_run() {
    let (controls, sup, log) = controls();
    let run = controls.deploy(&grok(), "hang on").expect("deployed");
    controls.stop(&grok()).await.expect("stopped");
    settle(&sup).await;
    assert_eq!(log.kinds(&run).last(), Some(&"run.cancelled"));
    assert_eq!(
        controls.stop(&grok()).await,
        Err(ControlError::NothingToStop(grok()))
    );
}

#[tokio::test(start_paused = true)]
async fn tp_skel_05_a_new_task_after_stop_closes_the_stopped_session_first() {
    let (controls, sup, log) = controls();
    let first = controls.deploy(&grok(), "hang on").expect("deployed");
    controls.stop(&grok()).await.expect("stopped");
    settle(&sup).await;
    let second = controls.deploy(&grok(), "hello").expect("deployed");
    settle(&sup).await;
    assert_eq!(log.kinds(&first).last(), Some(&"session.closed"));
    assert_ne!(
        log.queued(&first).session_id,
        log.queued(&second).session_id
    );
    log.assert_legal();
}

#[tokio::test(start_paused = true)]
async fn tp_skel_05_send_back_waits_for_the_run_to_end() {
    let (controls, _sup, _log) = controls();
    controls.deploy(&grok(), "hang on").expect("deployed");
    assert_eq!(controls.send_back(&grok()), Err(ControlError::Busy(grok())));
}

#[tokio::test(start_paused = true)]
async fn tp_skel_05_the_supervisor_closes_only_a_finished_session() {
    let log = Arc::new(MemLog::default());
    let sup = Supervisor::new(
        SupervisorConfig::default(),
        Arc::new(Script(by_prompt)),
        log.clone(),
        Arc::new(ManualClock::at(1_000)),
    );
    let request = |run: &str, prompt: &str| RunRequest {
        run_id: RunId::from(run),
        agent_id: grok(),
        session_id: SessionId::from(format!("s-{run}")),
        prompt: prompt.into(),
    };
    sup.deploy(request("working", "hang on")).expect("deploy");
    assert!(matches!(
        sup.close_session(&RunId::from("working")),
        Err(SupervisorError::Lifecycle(_))
    ));
    assert_eq!(
        sup.close_session(&RunId::from("nobody")),
        Err(SupervisorError::UnknownRun(RunId::from("nobody")))
    );
    sup.deploy(request("done", "hello")).expect("deploy");
    tokio::time::sleep(Duration::from_millis(10)).await;
    sup.close_session(&RunId::from("done")).expect("closed");
    assert_eq!(sup.phase(&RunId::from("done")), Some(Phase::Idle));
}

// ---- Time ------------------------------------------------------------------------------------

#[test]
fn the_system_clock_reads_wall_time() {
    let now = SystemClock.now_ms();
    // After 2026-01-01 and before 2100-01-01.
    assert!(now > 1_767_225_600_000 && now < 4_102_444_800_000, "{now}");
}

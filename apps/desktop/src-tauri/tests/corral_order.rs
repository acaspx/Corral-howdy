//! The corral's order and shown set in the app (M4 item 2, part 2, TP-USAGE-06..09). `CorralOrder`
//! keeps the core's `UsageOrder` and `ShownSet`, saved as `corral-order.json`, and arranges the
//! roster before it reaches Swift: the shown agents, most used first.
//!
//! Until Swift reports when the corral is idle (the next PR), a new order is published only at
//! launch, before anything can be under the pointer; agents still join and leave at once.

use corral_core::corral_view::AgentCard;
use corral_core::ids::AgentId;
use corral_core::usage_rank::{DAY_MS, Deploy};
use corral_desktop_lib::corral_order::{CorralOrder, MAX_ORDER_FILE_BYTES};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

const NOW: u64 = 1_000 * DAY_MS;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "corral-order-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).expect("clear scratch");
        }
        std::fs::create_dir_all(&dir).expect("scratch");
        Self(dir)
    }

    fn file(&self) -> PathBuf {
        self.0.join("corral-order.json")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort: a leftover temp folder only costs disk space.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn card(id: &str) -> AgentCard {
    AgentCard {
        id: AgentId::from(id),
        name: id.to_uppercase(),
        monogram: id[..1].to_uppercase(),
    }
}

fn cards(ids: &[&str]) -> Vec<AgentCard> {
    ids.iter().map(|id| card(id)).collect()
}

fn ids(cards: &[AgentCard]) -> Vec<&str> {
    cards.iter().map(|c| c.id.as_str()).collect()
}

fn deploys(agent: &str, n: u64) -> Vec<Deploy> {
    (0..n)
        .map(|i| Deploy {
            agent: AgentId::from(agent),
            ts_ms: NOW - i * 1_000,
        })
        .collect()
}

/// A launch: open the saved file, place the agents, seed the deploys, save.
fn launch(file: &Path, agents: &[&str], seen: Vec<Deploy>) -> (CorralOrder, Vec<AgentCard>) {
    let mut order = CorralOrder::open(Some(file.to_path_buf()));
    order.place(cards(agents), NOW);
    order.seed(seen, NOW);
    order.save().expect("saved");
    let placed = order.place(cards(agents), NOW).cards;
    (order, placed)
}

// ---- TP-USAGE-06: saved, and published at launch -----------------------------------------------

#[test]
fn tp_usage_06_a_first_launch_keeps_the_catalog_order_and_shows_everyone() {
    let dir = Scratch::new();
    let (order, placed) = launch(&dir.file(), &["grok", "claude", "gpt"], vec![]);
    assert_eq!(ids(&placed), ["grok", "claude", "gpt"]);
    assert_eq!(order.problem(), None);
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.file()).expect("file")).expect("json");
    assert_eq!(
        saved,
        serde_json::json!({"v": 1, "order": ["grok", "claude", "gpt"], "keepFixed": false,
                           "known": ["grok", "claude", "gpt"], "shown": ["grok", "claude", "gpt"]})
    );
}

#[test]
fn tp_usage_06_at_the_next_launch_the_most_used_comes_first() {
    let dir = Scratch::new();
    launch(&dir.file(), &["grok", "claude", "gpt"], vec![]);
    let (_, placed) = launch(&dir.file(), &["grok", "claude", "gpt"], deploys("gpt", 3));
    assert_eq!(ids(&placed), ["gpt", "grok", "claude"]);
    let (_, again) = launch(&dir.file(), &["grok", "claude", "gpt"], deploys("gpt", 3));
    assert_eq!(ids(&again), ["gpt", "grok", "claude"], "saved, and stable");
}

#[test]
fn tp_usage_06_keep_order_fixed_is_kept_across_launches() {
    let dir = Scratch::new();
    std::fs::write(
        dir.file(),
        r#"{"v": 1, "order": ["grok", "claude"], "keepFixed": true,
            "known": ["grok", "claude"], "shown": ["grok", "claude"]}"#,
    )
    .expect("write");
    let (order, placed) = launch(&dir.file(), &["grok", "claude"], deploys("claude", 5));
    assert!(order.keeps_fixed());
    assert_eq!(ids(&placed), ["grok", "claude"], "frozen");
}

// ---- TP-USAGE-07: while Corral runs --------------------------------------------------------------

#[test]
fn tp_usage_07_while_running_usage_waits_but_agents_join_and_leave_at_once() {
    let dir = Scratch::new();
    let (mut order, _) = launch(&dir.file(), &["grok", "claude"], vec![]);
    for deploy in deploys("claude", 4) {
        order.deployed(deploy.agent, deploy.ts_ms);
    }
    // Not published while the corral may be in use.
    order.run_ended(NOW);
    assert!(order.has_pending());
    assert_eq!(
        ids(&order.place(cards(&["grok", "claude"]), NOW).cards),
        ["grok", "claude"]
    );

    let placed = order.place(cards(&["grok", "claude", "muse"]), NOW);
    assert_eq!(
        ids(&placed.cards),
        ["grok", "claude", "muse"],
        "a new agent joins at the end"
    );
    assert!(!placed.choose_shown);
    let placed = order.place(cards(&["grok", "claude"]), NOW);
    assert_eq!(
        ids(&placed.cards),
        ["grok", "claude"],
        "and a removed one leaves"
    );

    assert!(
        order.idle(),
        "once the corral is idle, the waiting order is published"
    );
    assert_eq!(
        ids(&order.place(cards(&["grok", "claude"]), NOW).cards),
        ["claude", "grok"]
    );
}

#[test]
fn tp_usage_07_deploys_older_than_90_days_stop_counting() {
    let dir = Scratch::new();
    let old = vec![
        Deploy {
            agent: AgentId::from("claude"),
            ts_ms: NOW - 91 * DAY_MS,
        };
        5
    ];
    let (_, placed) = launch(&dir.file(), &["grok", "claude"], old);
    assert_eq!(ids(&placed), ["grok", "claude"]);
}

// ---- TP-USAGE-08: more than 8 -----------------------------------------------------------------

#[test]
fn tp_usage_08_a_ninth_agent_stays_hidden_and_asks() {
    let dir = Scratch::new();
    let eight = ["a", "b", "c", "d", "e", "f", "g", "h"];
    let (mut order, placed) = launch(&dir.file(), &eight, vec![]);
    assert_eq!(placed.len(), 8);
    let mut nine = eight.to_vec();
    nine.push("i");
    let placed = order.place(cards(&nine), NOW);
    assert_eq!(placed.cards.len(), 8, "the corral holds up to 8");
    assert!(!ids(&placed.cards).contains(&"i"));
    assert!(placed.choose_shown, "corral.choose_shown");
    assert!(order.needs_choice());
    assert_eq!(order.hidden_count(), 1);

    let placed = order.place(cards(&["b", "c", "d", "e", "f", "g", "h", "i"]), NOW);
    assert_eq!(
        placed.cards.len(),
        7,
        "removing a shown agent doesn't promote the hidden one"
    );
    assert!(!placed.choose_shown, "it only asks when an agent is added");
}

// ---- TP-USAGE-09: failing safe ----------------------------------------------------------------

#[test]
fn tp_usage_09_a_file_that_cant_be_read_starts_afresh_and_says_why() {
    let too_big = format!(
        "{{\"v\": 1, \"pad\": \"{}\"}}",
        "x".repeat(MAX_ORDER_FILE_BYTES)
    );
    for text in [
        "not json".to_owned(),
        r#"{"v": 2, "order": [], "keepFixed": false, "known": [], "shown": []}"#.to_owned(),
        r#"{"v": 1, "order": [], "keepFixed": false, "known": [], "shown": [], "extra": 1}"#
            .to_owned(),
        too_big,
    ] {
        let dir = Scratch::new();
        std::fs::write(dir.file(), &text).expect("write");
        let mut order = CorralOrder::open(Some(dir.file()));
        let problem = order.problem().map(str::to_owned).expect("a problem");
        assert!(
            problem.starts_with("Couldn't read the corral's saved order"),
            "{problem}"
        );
        let placed = order.place(cards(&["grok", "claude"]), NOW);
        assert_eq!(ids(&placed.cards), ["grok", "claude"], "{text:.30}");
    }
}

#[test]
fn tp_usage_09_without_a_folder_it_still_works_and_saving_is_a_no_op() {
    let mut order = CorralOrder::open(None);
    assert_eq!(ids(&order.place(cards(&["grok"]), NOW).cards), ["grok"]);
    order.save().expect("nothing to save to");
}

#[test]
fn tp_usage_09_a_save_that_fails_says_why() {
    let dir = Scratch::new();
    std::fs::create_dir_all(dir.file()).expect("a folder where the file goes");
    let mut order = CorralOrder::open(Some(dir.file()));
    order.place(cards(&["grok"]), NOW);
    let err = order.save().expect_err("can't write");
    assert!(err.starts_with("Couldn't save the corral's order"), "{err}");
}

#[test]
fn tp_usage_06_the_saved_order_and_shown_set_are_restored() {
    let dir = Scratch::new();
    std::fs::write(
        dir.file(),
        r#"{"v": 1, "order": ["claude", "grok", "gpt"], "keepFixed": false,
            "known": ["grok", "claude", "gpt"], "shown": ["claude", "grok"]}"#,
    )
    .expect("write");
    let mut order = CorralOrder::open(Some(dir.file()));
    assert_eq!(order.problem(), None);
    assert!(!order.keeps_fixed() && !order.has_pending() && !order.needs_choice());
    let placed = order.place(cards(&["grok", "claude", "gpt"]), NOW);
    assert_eq!(
        ids(&placed.cards),
        ["claude", "grok"],
        "the saved order, the saved shown set"
    );
    assert_eq!(order.hidden_count(), 1);
    assert!(!placed.choose_shown);
}

#[test]
fn tp_usage_06_a_saved_fixed_order_holds_against_usage() {
    let dir = Scratch::new();
    std::fs::write(
        dir.file(),
        r#"{"v": 1, "order": ["claude", "grok"], "keepFixed": true,
            "known": ["grok", "claude"], "shown": ["grok", "claude"]}"#,
    )
    .expect("write");
    let (order, placed) = launch(&dir.file(), &["grok", "claude"], deploys("grok", 5));
    assert!(order.keeps_fixed());
    assert_eq!(ids(&placed), ["claude", "grok"]);
    assert_eq!(order.hidden_count(), 0);
}

#[test]
fn tp_usage_09_no_file_is_no_problem_but_an_unreadable_one_is() {
    let dir = Scratch::new();
    assert_eq!(
        CorralOrder::open(Some(dir.file())).problem(),
        None,
        "a first launch"
    );
    std::fs::create_dir_all(dir.file()).expect("a folder where the file goes");
    assert!(
        CorralOrder::open(Some(dir.file())).problem().is_some(),
        "can't be read"
    );
}

#[test]
fn tp_usage_09_the_limits_are_exact() {
    assert_eq!(MAX_ORDER_FILE_BYTES, 16 * 1024);
    let file = |agents: usize, size: Option<usize>| {
        let list: Vec<String> = (0..agents).map(|n| format!("\"a{n}\"")).collect();
        let mut text = format!(
            r#"{{"v": 1, "order": [{}], "keepFixed": false, "known": [], "shown": []}}"#,
            list.join(",")
        );
        if let Some(size) = size {
            text.push_str(&" ".repeat(size.saturating_sub(text.len())));
        }
        text
    };
    for (text, readable) in [
        (file(0, Some(MAX_ORDER_FILE_BYTES)), true),
        (file(0, Some(MAX_ORDER_FILE_BYTES + 1)), false),
        (file(256, None), true),
        (file(257, None), false),
    ] {
        let dir = Scratch::new();
        std::fs::write(dir.file(), &text).expect("write");
        assert_eq!(
            CorralOrder::open(Some(dir.file())).problem().is_none(),
            readable,
            "{} bytes",
            text.len()
        );
    }
}

fn saved(file: &Path) -> serde_json::Value {
    serde_json::from_slice(&std::fs::read(file).expect("file")).expect("json")
}

#[test]
fn tp_usage_06_the_order_a_launch_publishes_is_saved() {
    let dir = Scratch::new();
    launch(&dir.file(), &["grok", "claude", "gpt"], vec![]);
    launch(&dir.file(), &["grok", "claude", "gpt"], deploys("gpt", 3));
    assert_eq!(
        saved(&dir.file())["order"],
        serde_json::json!(["gpt", "grok", "claude"])
    );
}

#[test]
fn tp_usage_07_an_order_published_when_idle_is_saved_and_a_removed_agent_is_forgotten() {
    let dir = Scratch::new();
    let (mut order, _) = launch(&dir.file(), &["grok", "claude", "gpt"], vec![]);
    for deploy in deploys("claude", 4) {
        order.deployed(deploy.agent, deploy.ts_ms);
    }
    order.run_ended(NOW);
    assert!(order.idle());
    order.save().expect("saved");
    assert_eq!(
        saved(&dir.file())["order"],
        serde_json::json!(["claude", "grok", "gpt"])
    );

    order.place(cards(&["claude", "grok"]), NOW);
    order.save().expect("saved");
    assert_eq!(
        saved(&dir.file())["known"],
        serde_json::json!(["grok", "claude"])
    );
    assert_eq!(order.hidden_count(), 0);
}

// ---- TP-USAGE-12: publishing when the corral is idle ------------------------------------------

fn waiting_order(dir: &Scratch) -> CorralOrder {
    let (mut order, _) = launch(&dir.file(), &["grok", "claude"], vec![]);
    for deploy in deploys("claude", 4) {
        order.deployed(deploy.agent, deploy.ts_ms);
    }
    order.run_ended(NOW);
    assert!(order.has_pending());
    order
}

#[test]
fn tp_usage_12_a_waiting_order_is_published_only_when_swift_says_idle_and_nothing_waits_on_you() {
    let dir = Scratch::new();
    let mut order = waiting_order(&dir);
    assert!(
        !order.settle(false),
        "Rust assumes busy until Swift reports"
    );
    assert!(order.has_pending());

    order.set_corral_idle(true);
    assert!(!order.settle(true), "a checkpoint is waiting on the user");
    assert!(order.has_pending());

    order.set_corral_idle(false);
    assert!(!order.settle(false), "the pointer is on the corral");

    order.set_corral_idle(true);
    assert!(order.settle(false), "idle: published");
    assert!(!order.has_pending());
    assert_eq!(
        ids(&order.place(cards(&["grok", "claude"]), NOW).cards),
        ["claude", "grok"]
    );
    order.save().expect("saved");
    assert_eq!(
        saved(&dir.file())["order"],
        serde_json::json!(["claude", "grok"])
    );
    assert!(!order.settle(false), "nothing more to publish");
}

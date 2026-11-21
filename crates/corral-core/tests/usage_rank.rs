//! The corral's order and shown set (M4 item 2, ADR-012, docs/07 §3.1-3.2, TP-USAGE-01..05).
//! Pure core logic: the app feeds it deploys, the agents and whether the corral is idle, and hands
//! Swift the shown agents most used first; Swift only maps rank to slot (TP-RANK-*).

use corral_core::event::*;
use corral_core::ids::{AgentId, RunId, SessionId, SpanId};
use corral_core::usage_rank::{
    DAY_MS, Deploy, HALF_LIFE_MS, HYSTERESIS, MAX_SHOWN, ShownChange, ShownError, ShownSet,
    UsageOrder, WINDOW_MS, deploys, rank, reorder, score,
};
use proptest::prelude::*;

const NOW: u64 = 1_000 * DAY_MS;

fn id(name: &str) -> AgentId {
    AgentId::from(name)
}

fn ids(names: &[&str]) -> Vec<AgentId> {
    names.iter().map(|n| id(n)).collect()
}

fn deploy(agent: &str, days_ago: u64) -> Deploy {
    Deploy {
        agent: id(agent),
        ts_ms: NOW - days_ago * DAY_MS,
    }
}

fn names(order: &[AgentId]) -> Vec<&str> {
    order.iter().map(AgentId::as_str).collect()
}

// ---- TP-USAGE-01: the score and its tie-breaks -----------------------------------------------

#[test]
fn tp_usage_01_constants_are_the_documented_ones() {
    assert_eq!(DAY_MS, 86_400_000);
    assert_eq!(WINDOW_MS, 90 * DAY_MS);
    assert_eq!(HALF_LIFE_MS, 14 * DAY_MS);
    assert!((HYSTERESIS - 1.10).abs() < f64::EPSILON);
    assert_eq!(MAX_SHOWN, 8);
}

#[test]
fn tp_usage_01_each_deploy_counts_less_every_14_days_and_not_after_90() {
    let close = |a: f64, b: f64| (a - b).abs() < 1e-9;
    assert!(close(score(&[deploy("a", 0)], NOW), 1.0));
    assert!(close(score(&[deploy("a", 14)], NOW), 0.5));
    assert!(close(score(&[deploy("a", 28)], NOW), 0.25));
    assert!(close(score(&[deploy("a", 0), deploy("a", 14)], NOW), 1.5));
    assert!(score(&[deploy("a", 90)], NOW) > 0.0, "90 days still counts");
    assert!(
        close(score(&[deploy("a", 91)], NOW), 0.0),
        "older than 90 days doesn't"
    );
    assert!(close(score(&[], NOW), 0.0));
    let future = Deploy {
        agent: id("a"),
        ts_ms: NOW + DAY_MS,
    };
    assert!(
        close(score(&[future], NOW), 1.0),
        "a clock ahead counts as now, never more"
    );
}

#[test]
fn tp_usage_01_rank_by_score_then_last_deploy_then_the_order_added() {
    let agents = ids(&["grok", "claude", "gpt", "muse"]);
    let ranked = rank(
        &agents,
        &[
            deploy("claude", 1),
            deploy("claude", 2),
            deploy("gpt", 0),
            deploy("grok", 14),
            deploy("grok", 14),
        ],
        NOW,
    );
    // claude ≈ 1.9, gpt = 1.0, grok = 1.0 (two at half weight): gpt deployed later than grok.
    assert_eq!(
        ranked.iter().map(|r| r.agent.as_str()).collect::<Vec<_>>(),
        ["claude", "gpt", "grok", "muse"]
    );
    assert_eq!(ranked[1].last_deploy_ms, Some(NOW));
    assert_eq!(ranked[3].last_deploy_ms, None);
    assert!(ranked[3].score.abs() < f64::EPSILON);

    let untouched = rank(&ids(&["b", "a", "c"]), &[], NOW);
    assert_eq!(
        untouched
            .iter()
            .map(|r| r.agent.as_str())
            .collect::<Vec<_>>(),
        ["b", "a", "c"],
        "no deploys: the order they were added"
    );
    let strangers = rank(&ids(&["a"]), &[deploy("gone", 0)], NOW);
    assert_eq!(
        strangers.len(),
        1,
        "deploys of agents not in the corral are ignored"
    );
}

#[test]
fn tp_usage_01_deploys_are_the_runs_queued() {
    let event = |seq: u64, ts: u64, body: EventBody| Event {
        run_id: RunId::from(format!("r{seq}").as_str()),
        seq: 1,
        ts,
        span_id: SpanId::from("s"),
        parent_span_id: None,
        body,
    };
    let queued = |agent: &str| {
        EventBody::RunQueued(RunQueued {
            agent_id: id(agent),
            session_id: SessionId::from("s1"),
            prompt: "hi".into(),
        })
    };
    let events = vec![
        event(1, 10, queued("grok")),
        event(
            2,
            20,
            EventBody::RunCompleted(RunCompleted { summary: None }),
        ),
        event(3, 30, queued("claude")),
    ];
    assert_eq!(
        deploys(&events),
        [
            Deploy {
                agent: id("grok"),
                ts_ms: 10
            },
            Deploy {
                agent: id("claude"),
                ts_ms: 30
            },
        ]
    );
}

// ---- TP-USAGE-02: published only when the corral is idle -------------------------------------

#[test]
fn tp_usage_02_a_new_order_waits_until_the_corral_is_idle() {
    let agents = ids(&["grok", "claude"]);
    let mut order = UsageOrder::new(agents.clone(), false);
    let busy = rank(&agents, &[deploy("claude", 0), deploy("claude", 1)], NOW);

    assert!(!order.update(&busy, false), "busy: nothing moves");
    assert_eq!(names(order.order()), ["grok", "claude"]);
    assert!(order.has_pending());

    assert!(order.idle(), "idle: the waiting order is published");
    assert_eq!(names(order.order()), ["claude", "grok"]);
    assert!(!order.has_pending());
    assert!(!order.idle(), "nothing more to publish");

    let mut direct = UsageOrder::new(agents.clone(), false);
    assert!(
        direct.update(&busy, true),
        "already idle: published at once"
    );
    assert_eq!(names(direct.order()), ["claude", "grok"]);
}

#[test]
fn tp_usage_02_agents_join_and_leave_at_once_without_moving_the_others() {
    let mut order = UsageOrder::new(ids(&["grok", "claude"]), false);
    // gpt is new and most used, claude is gone; the corral is busy.
    let ranked = rank(
        &ids(&["grok", "gpt"]),
        &[deploy("gpt", 0), deploy("gpt", 0)],
        NOW,
    );
    assert!(order.update(&ranked, false), "membership changed");
    assert_eq!(
        names(order.order()),
        ["grok", "gpt"],
        "a new agent joins at the end; the order waits for idle"
    );
    assert!(order.idle());
    assert_eq!(names(order.order()), ["gpt", "grok"]);
}

// ---- TP-USAGE-03: hysteresis ----------------------------------------------------------------

#[test]
fn tp_usage_03_a_neighbor_moves_up_only_when_it_leads_by_ten_percent() {
    let published = ids(&["a", "b"]);
    let scored = |a: f64, b: f64| {
        vec![
            corral_core::usage_rank::Ranked {
                agent: id("a"),
                score: a,
                last_deploy_ms: Some(NOW),
            },
            corral_core::usage_rank::Ranked {
                agent: id("b"),
                score: b,
                last_deploy_ms: Some(NOW),
            },
        ]
    };
    assert_eq!(names(&reorder(&published, &scored(1.0, 1.05))), ["a", "b"]);
    assert_eq!(names(&reorder(&published, &scored(1.0, 1.09))), ["a", "b"]);
    assert_eq!(names(&reorder(&published, &scored(1.0, 1.10))), ["b", "a"]);
    assert_eq!(names(&reorder(&published, &scored(1.0, 2.0))), ["b", "a"]);
    assert_eq!(
        names(&reorder(&published, &scored(0.0, 0.0))),
        ["a", "b"],
        "zeros don't swap"
    );
    assert_eq!(
        names(&reorder(&published, &scored(0.0, 0.01))),
        ["b", "a"],
        "any use beats none"
    );
    let three = vec![
        corral_core::usage_rank::Ranked {
            agent: id("a"),
            score: 1.0,
            last_deploy_ms: None,
        },
        corral_core::usage_rank::Ranked {
            agent: id("b"),
            score: 2.0,
            last_deploy_ms: None,
        },
        corral_core::usage_rank::Ranked {
            agent: id("c"),
            score: 4.0,
            last_deploy_ms: None,
        },
    ];
    assert_eq!(
        names(&reorder(&ids(&["a", "b", "c"]), &three)),
        ["c", "b", "a"],
        "a clear leader climbs all the way"
    );
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 512, ..ProptestConfig::default() })]

    /// Whatever the scores and the published order: the result has every current agent once, is
    /// stable (applying it again changes nothing, so near-ties never flip back and forth), and no
    /// agent sits right behind one it leads by 10% or more.
    #[test]
    fn tp_usage_03_reordering_is_stable_and_settled(
        scores in prop::collection::vec(0.0f64..20.0, 1..9),
        shuffle in prop::collection::vec(any::<u32>(), 9),
    ) {
        let agents: Vec<AgentId> = (0..scores.len()).map(|n| id(&format!("a{n}"))).collect();
        let ranked: Vec<corral_core::usage_rank::Ranked> = agents
            .iter()
            .zip(&scores)
            .map(|(agent, score)| corral_core::usage_rank::Ranked {
                agent: agent.clone(),
                score: *score,
                last_deploy_ms: None,
            })
            .collect();
        let mut published = agents.clone();
        published.sort_by_key(|a| shuffle[a.as_str()[1..].parse::<usize>().unwrap_or(0)]);

        let once = reorder(&published, &ranked);
        let mut sorted = once.clone();
        sorted.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        let mut expected = agents.clone();
        expected.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        prop_assert_eq!(sorted, expected, "every agent once");
        prop_assert_eq!(reorder(&once, &ranked), once.clone(), "stable");
        let score_of = |a: &AgentId| ranked.iter().find(|r| &r.agent == a).map(|r| r.score).unwrap_or(0.0);
        for pair in once.windows(2) {
            let (ahead, behind) = (score_of(&pair[0]), score_of(&pair[1]));
            prop_assert!(
                !(behind > ahead && behind >= ahead * HYSTERESIS),
                "{} ({behind}) should have passed {} ({ahead})",
                pair[1].as_str(),
                pair[0].as_str()
            );
        }
    }
}

// ---- TP-USAGE-04: "Keep order fixed" ---------------------------------------------------------

#[test]
fn tp_usage_04_keep_order_fixed_freezes_the_order_but_not_the_score() {
    let agents = ids(&["grok", "claude"]);
    let deploys_now = [deploy("claude", 0), deploy("claude", 0)];
    let mut order = UsageOrder::new(agents.clone(), true);
    assert!(order.keeps_fixed());
    let ranked = rank(&agents, &deploys_now, NOW);
    assert!(
        ranked[0].score > 1.9,
        "deploys still count: {}",
        ranked[0].score
    );
    assert!(!order.update(&ranked, true));
    assert_eq!(names(order.order()), ["grok", "claude"], "frozen");

    let with_gpt = rank(&ids(&["grok", "claude", "gpt"]), &deploys_now, NOW);
    assert!(order.update(&with_gpt, true));
    assert_eq!(
        names(order.order()),
        ["grok", "claude", "gpt"],
        "new agents still join, at the end"
    );

    order.set_keep_fixed(false);
    assert!(order.update(&with_gpt, true));
    assert_eq!(
        names(order.order())[0],
        "claude",
        "unfrozen, it follows usage again"
    );
}

// ---- TP-USAGE-05: the shown set ---------------------------------------------------------------

#[test]
fn tp_usage_05_the_first_eight_show_and_a_ninth_asks() {
    let mut shown = ShownSet::default();
    for n in 0..8 {
        assert_eq!(shown.add(id(&format!("a{n}"))), ShownChange::Shown);
    }
    assert_eq!(shown.add(id("a3")), ShownChange::Unchanged, "already there");
    assert_eq!(
        shown.add(id("a8")),
        ShownChange::ChooseShown,
        "corral.choose_shown"
    );
    assert_eq!(shown.shown().len(), 8, "changes nothing until answered");
    assert!(!shown.is_shown(&id("a8")));
    assert!(
        shown.is_known(&id("a8")),
        "known, and listed in the chooser"
    );
    assert!(shown.needs_choice());

    let picks = ids(&["a8", "a0", "a1", "a2", "a3", "a4", "a5", "a6"]);
    shown.choose(&picks).expect("eight picks");
    assert!(shown.is_shown(&id("a8")) && !shown.is_shown(&id("a7")));
    assert!(!shown.needs_choice());
}

#[test]
fn tp_usage_05_removing_a_shown_agent_never_promotes_a_hidden_one() {
    let mut shown = ShownSet::default();
    for n in 0..9 {
        shown.add(id(&format!("a{n}")));
    }
    assert!(!shown.is_shown(&id("a8")));
    shown.remove(&id("a0"));
    assert_eq!(shown.shown().len(), 7, "the corral shrinks");
    assert!(
        !shown.is_shown(&id("a8")),
        "a8 stays hidden until the user picks it"
    );
    assert!(!shown.is_known(&id("a0")));
    assert_eq!(
        shown.add(id("a9")),
        ShownChange::Shown,
        "room again for a new one"
    );
}

#[test]
fn tp_usage_05_choosing_is_checked() {
    let mut shown = ShownSet::default();
    for n in 0..9 {
        shown.add(id(&format!("a{n}")));
    }
    let nine = ids(&["a0", "a1", "a2", "a3", "a4", "a5", "a6", "a7", "a8"]);
    assert_eq!(shown.choose(&nine), Err(ShownError::TooMany(9)));
    assert_eq!(
        shown.choose(&ids(&["a0", "stranger"])),
        Err(ShownError::Unknown(id("stranger")))
    );
    shown
        .choose(&ids(&["a1", "a1", "a2"]))
        .expect("duplicates count once");
    assert_eq!(shown.shown().len(), 2);
    shown.choose(&[]).expect("an empty corral is a choice");
    assert!(shown.shown().is_empty());
    assert!(!shown.needs_choice(), "the user answered");
}

#[test]
fn tp_usage_05_swift_gets_the_shown_agents_most_used_first() {
    let mut shown = ShownSet::default();
    for name in ["a", "b", "c"] {
        shown.add(id(name));
    }
    shown.choose(&ids(&["a", "c"])).expect("two");
    let order = ids(&["c", "b", "a"]);
    assert_eq!(names(&shown.in_order(&order)), ["c", "a"]);
    assert_eq!(shown.hidden_count(), 1);
    let restored = ShownSet::restore(ids(&["a", "b", "c"]), ids(&["c"]));
    assert_eq!(names(&restored.in_order(&order)), ["c"]);
    assert_eq!(
        ShownSet::restore(ids(&["a"]), ids(&["a", "zz"])).shown(),
        ids(&["a"]),
        "a saved shown agent that's gone is dropped"
    );
}

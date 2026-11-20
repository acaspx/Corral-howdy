//! The main window's switch for provider-side tools (M3 item 4.2, TP-PROV-11, 12, 14). The choice
//! is kept in Corral's own folder as `provider-tools.json`, written whole or not at all; a file
//! that can't be read leaves every tool off and says why. An agent given a tool runs under a
//! policy that allows `provider_tools`, and nothing else about its policy changes.

use corral_adapters::agent::{ProviderTool, allows};
use corral_core::domain::ActionClass;
use corral_core::ids::AgentId;
use corral_desktop_lib::agent_tools::{allowing_provider_tools, file_tools};
use corral_desktop_lib::provider_tools::{MAX_PROVIDER_TOOLS_BYTES, ProviderTools};
use corral_policy::{Action, Decision, RunGrants};
use corral_tools::CheckpointDesk;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

/// A fresh folder, cleaned up on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "corral-provider-tools-{}-{}",
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
        self.0.join("provider-tools.json")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort: a leftover temp folder only costs disk space.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn claude() -> AgentId {
    AgentId::from("claude")
}

// ---- TP-PROV-11: the choice, saved ----------------------------------------------------------

#[test]
fn tp_prov_11_claudes_search_is_offered_and_off_until_turned_on() {
    let dir = Scratch::new();
    let tools = ProviderTools::open(dir.file());
    let view = tools.view();
    assert_eq!(view.problem, None);
    assert_eq!(
        serde_json::to_value(&view.choices).expect("json"),
        json!([{"agent": "claude", "agentName": "Claude", "tool": "web_search",
                "runsOn": "Anthropic", "costMicroUsd": 10_000, "perTurn": 5, "on": false}])
    );
    assert!(!tools.is_on(&claude(), ProviderTool::WebSearch));
    assert!(
        !dir.file().exists(),
        "nothing is written until a choice is made"
    );
}

#[test]
fn tp_prov_11_turning_it_on_is_saved_and_read_back() {
    let dir = Scratch::new();
    let tools = ProviderTools::open(dir.file());
    let view = tools
        .set("claude", ProviderTool::WebSearch, true)
        .expect("saved");
    assert!(view.choices[0].on);
    assert!(tools.is_on(&claude(), ProviderTool::WebSearch));
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.file()).expect("file")).expect("json");
    assert_eq!(saved, json!({"claude": ["web_search"]}));

    let reopened = ProviderTools::open(dir.file());
    assert!(reopened.is_on(&claude(), ProviderTool::WebSearch));
    assert_eq!(reopened.view().problem, None);

    let view = reopened
        .set("claude", ProviderTool::WebSearch, false)
        .expect("saved");
    assert!(!view.choices[0].on);
    assert!(!ProviderTools::open(dir.file()).is_on(&claude(), ProviderTool::WebSearch));
    let names: Vec<String> = std::fs::read_dir(&dir.0)
        .expect("dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(names, ["provider-tools.json"], "no temp file left behind");
}

#[test]
fn tp_prov_11_a_tool_an_agent_isnt_offered_is_refused_and_nothing_is_saved() {
    let dir = Scratch::new();
    let tools = ProviderTools::open(dir.file());
    for agent in ["grok", "gpt", "muse-spark", "stranger", ""] {
        let err = tools
            .set(agent, ProviderTool::WebSearch, true)
            .expect_err("refused");
        assert!(err.contains("can't be given web search"), "{agent}: {err}");
        assert!(!tools.is_on(&AgentId::from(agent), ProviderTool::WebSearch));
    }
    assert!(!dir.file().exists());
}

// ---- TP-PROV-12: failing closed ---------------------------------------------------------------

#[test]
fn tp_prov_12_a_file_that_cant_be_read_leaves_everything_off_and_says_why() {
    let too_big = format!(
        "{{\"claude\": [\"web_search\"], \"pad\": \"{}\"}}",
        "x".repeat(MAX_PROVIDER_TOOLS_BYTES)
    );
    for text in [
        "not json".to_owned(),
        r#"{"claude": ["code_execution"]}"#.to_owned(),
        r#"{"grok": ["web_search"]}"#.to_owned(),
        r#"{"claude": "web_search"}"#.to_owned(),
        too_big,
    ] {
        let dir = Scratch::new();
        std::fs::write(dir.file(), &text).expect("write");
        let tools = ProviderTools::open(dir.file());
        assert!(
            !tools.is_on(&claude(), ProviderTool::WebSearch),
            "{text:.40}"
        );
        let view = tools.view();
        assert!(view.choices.iter().all(|c| !c.on), "{text:.40}");
        let problem = view.problem.expect("a problem");
        assert!(
            problem.starts_with("Couldn't read your provider tool choices, so they're all off"),
            "{problem}"
        );
        assert_eq!(
            std::fs::read_to_string(dir.file()).expect("kept"),
            text,
            "the file is left for the user to see"
        );
    }
}

#[test]
fn tp_prov_12_a_choice_that_cant_be_saved_isnt_made() {
    let dir = Scratch::new();
    // A folder where the file should be: nothing can be written there.
    std::fs::create_dir_all(dir.file()).expect("folder");
    let tools = ProviderTools::open(dir.file());
    let err = tools
        .set("claude", ProviderTool::WebSearch, true)
        .expect_err("not saved");
    assert!(err.starts_with("Couldn't save your choice"), "{err}");
    assert!(
        !tools.is_on(&claude(), ProviderTool::WebSearch),
        "an unsaved choice doesn't take effect"
    );
}

#[test]
fn tp_prov_12_a_fixed_file_clears_the_problem_once_a_choice_is_saved() {
    let dir = Scratch::new();
    std::fs::write(dir.file(), "not json").expect("write");
    let tools = ProviderTools::open(dir.file());
    assert!(tools.view().problem.is_some());
    let view = tools
        .set("claude", ProviderTool::WebSearch, true)
        .expect("saved");
    assert_eq!(view.problem, None);
    assert!(view.choices[0].on);
}

// ---- TP-PROV-14: the policy an agent with provider tools runs under ----------------------------

#[test]
fn tp_prov_14_only_provider_tools_change_in_the_policy() {
    let home = Scratch::new();
    let setup =
        file_tools(&home.0.join("Claude"), Arc::new(CheckpointDesk::default())).expect("tools");
    assert!(
        !allows(&setup.policy, ProviderTool::WebSearch),
        "off by default"
    );
    let searching = allowing_provider_tools(setup.clone()).expect("policy");
    assert!(allows(&searching.policy, ProviderTool::WebSearch));
    for class in [
        ActionClass::ReadLocal,
        ActionClass::WriteLocal,
        ActionClass::DeleteLocal,
        ActionClass::NetWrite,
        ActionClass::CommsSend,
        ActionClass::MoneySpend,
        ActionClass::ExecShell,
        ActionClass::Unknown,
    ] {
        let action = Action::new(class, "a.tool");
        assert_eq!(
            searching.policy.decide(&action, &RunGrants::default()),
            setup.policy.decide(&action, &RunGrants::default()),
            "{class:?} decides as before"
        );
    }
    assert!(matches!(
        searching.policy.decide(
            &Action::new(ActionClass::WriteLocal, "fs.write"),
            &RunGrants::default()
        ),
        Decision::Ask { .. }
    ));
    assert!(
        Arc::ptr_eq(&searching.gateway, &setup.gateway),
        "the same tools"
    );
    assert!(Arc::ptr_eq(&searching.desk, &setup.desk), "the same desk");
}

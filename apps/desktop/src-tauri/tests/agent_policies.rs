//! Each agent's policy, saved (M4 item 6; TP-PED-05..06): `<app data>/policies/<agent>.toml`,
//! read at launch. No file means the defaults; a file that's there but can't be used is an
//! error, and the agent then gets no tools, so a broken file never quietly loosens a rule
//! (AGENTS.md: checks fail closed). Its file tools run under the policy it loaded.

use corral_core::domain::ActionClass;
use corral_core::ids::AgentId;
use corral_desktop_lib::agent_policies::{AgentPolicies, MAX_POLICY_FILE_BYTES};
use corral_desktop_lib::agent_tools::{allowing_provider_tools, file_tools_under};
use corral_policy::{Policy, Profile, ProfileEdit, Rule};
use corral_tools::CheckpointDesk;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A fresh scratch folder, cleaned up on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "corral-policies-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path).expect("clear");
        }
        std::fs::create_dir_all(&path).expect("scratch");
        Self(path)
    }

    fn policies(&self) -> AgentPolicies {
        AgentPolicies::new(self.0.join("policies"))
    }

    fn file(&self, agent: &str) -> PathBuf {
        self.0.join("policies").join(format!("{agent}.toml"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Best effort: a leftover temp folder only costs disk space.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn grok() -> AgentId {
    AgentId::from("grok")
}

fn edited() -> Policy {
    let start = Policy::new(Profile::default());
    let mut edit = ProfileEdit::from(start.profile());
    edit.rules = vec![
        (ActionClass::WriteLocal, Some(Rule::Allow)),
        (ActionClass::NetWrite, Some(Rule::Deny)),
    ];
    edit.net_allow = vec!["*.wikipedia.org".into()];
    edit.loop_limit = 9;
    start.edited(&edit).expect("valid")
}

// ---- TP-PED-05: saved and read back, failing closed ---------------------------------------

#[test]
fn tp_ped_05_no_file_means_the_defaults() {
    let s = Scratch::new();
    assert_eq!(
        s.policies().load(&grok()),
        Ok(Policy::new(Profile::default()))
    );
}

#[test]
fn tp_ped_05_a_saved_policy_reads_back_the_same_for_its_agent_only() {
    let s = Scratch::new();
    let policies = s.policies();
    policies.save(&grok(), &edited()).expect("save");
    assert_eq!(policies.load(&grok()), Ok(edited()));
    assert_eq!(
        policies.load(&AgentId::from("claude")),
        Ok(Policy::new(Profile::default())),
        "each agent has its own"
    );
    let leftovers: Vec<_> = std::fs::read_dir(s.0.join("policies"))
        .expect("dir")
        .map(|e| e.expect("entry").file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(
        leftovers,
        ["grok.toml"],
        "written through a temp file and a rename"
    );
}

#[test]
fn tp_ped_05_a_file_that_cant_be_used_is_an_error_not_the_defaults() {
    let s = Scratch::new();
    std::fs::create_dir_all(s.0.join("policies")).expect("dir");
    for (text, why) in [
        ("loop_limit = \"many\"\n", "a wrong type"),
        ("colour = \"blue\"\n", "an unknown key"),
        (
            "[[rules]]\nclass = \"unknown\"\ndecision = \"allow\"\n",
            "unknown opened",
        ),
        (
            "[[rules]]\nclass = \"money.spend\"\ndecision = \"allow\"\n",
            "over a ceiling",
        ),
        ("net_allow = [\"https://example.com\"]\n", "a bad domain"),
    ] {
        std::fs::write(s.file("grok"), text).expect("write");
        let error = s.policies().load(&grok()).expect_err(why);
        assert!(error.contains("grok.toml"), "{why}: {error}");
    }
    std::fs::write(s.file("grok"), "#".repeat(MAX_POLICY_FILE_BYTES + 1)).expect("write");
    assert!(s.policies().load(&grok()).is_err(), "too big");
}

#[test]
fn tp_ped_05_an_agent_id_never_leaves_the_policies_folder() {
    let s = Scratch::new();
    for id in [
        "",
        "../grok",
        "a/b",
        "a\\b",
        ".hidden",
        "grok.toml",
        "Grok Bot",
    ] {
        let agent = AgentId::from(id);
        assert!(s.policies().load(&agent).is_err(), "{id:?}");
        assert!(s.policies().save(&agent, &edited()).is_err(), "{id:?}");
    }
    assert!(!s.0.join("grok.toml").exists());
    assert!(!s.0.join("policies").exists(), "nothing written");
}

// ---- TP-PED-06: the agent's tools run under the policy it loaded ---------------------------

#[test]
fn tp_ped_06_file_tools_run_under_the_agents_policy_and_provider_tools_add_to_it() {
    let s = Scratch::new();
    let folder = s.0.join("Grok");
    let setup =
        file_tools_under(&folder, Arc::new(CheckpointDesk::default()), edited()).expect("tools");
    assert_eq!(setup.policy, edited());
    let searching = allowing_provider_tools(setup).expect("allowed");
    let profile = searching.policy.profile();
    assert_eq!(profile.rule(ActionClass::ProviderTools), Some(Rule::Allow));
    assert_eq!(
        profile.rule(ActionClass::WriteLocal),
        Some(Rule::Allow),
        "the agent's own rules stay"
    );
    assert_eq!(profile.loop_limit, 9);
}

#[test]
fn tp_ped_05_a_file_that_cant_be_read_is_an_error_too() {
    let s = Scratch::new();
    std::fs::create_dir_all(s.file("grok")).expect("a folder where the file should be");
    let error = s.policies().load(&grok()).expect_err("unreadable");
    assert!(error.contains("grok.toml"), "{error}");
}

#[test]
fn tp_ped_05_a_file_of_exactly_the_most_bytes_is_read() {
    let s = Scratch::new();
    std::fs::create_dir_all(s.0.join("policies")).expect("dir");
    std::fs::write(s.file("grok"), "#".repeat(MAX_POLICY_FILE_BYTES)).expect("write");
    assert_eq!(
        s.policies().load(&grok()),
        Ok(Policy::new(Profile::default())),
        "a file of comments is the defaults"
    );
}

#[test]
fn tp_ped_05_ids_with_dashes_and_underscores_are_plain_names() {
    let s = Scratch::new();
    for id in ["my-agent", "my_agent", "agent-2_b"] {
        let agent = AgentId::from(id);
        s.policies().save(&agent, &edited()).expect(id);
        assert_eq!(s.policies().load(&agent), Ok(edited()), "{id}");
    }
}

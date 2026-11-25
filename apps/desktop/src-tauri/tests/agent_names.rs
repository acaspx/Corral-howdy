//! Agents' names, saved and shown (M4 item 6; TP-ANM-02..04). A rename is kept in the agent's
//! settings file, next to its model and cap, and the app's one lookup, `AgentNames`, gives every
//! place that names an agent the same name: the corral's cards (with a monogram from the name),
//! the main window's sections, and the agent's folder, `~/Corral/<name>`.
//!
//! Fail closed: a saved name that can't be used, or that is another agent's name, is dropped,
//! the agent keeps its catalog name, its own folder, and the problem says why. No agent can be
//! given another agent's catalog name, so its default folder stays its own.

use corral_adapters::catalog::{agent_for, model_choice};
use corral_core::agent_name::AgentName;
use corral_core::corral_view::AgentCard;
use corral_core::event::{Event, EventBody, RunQueued};
use corral_core::ids::{AgentId, RunId, SessionId, SpanId};
use corral_desktop_lib::agent_names::AgentNames;
use corral_desktop_lib::agent_policies::PolicyEditor;
use corral_desktop_lib::agent_settings::{AgentSettings, AgentSetup};
use corral_desktop_lib::provider_tools::ProviderTools;
use corral_desktop_lib::traces::Traces;
use corral_secrets::Provider;
use corral_store::Store;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// A fresh scratch folder, cleaned up on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "corral-agent-names-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path).expect("clear");
        }
        std::fs::create_dir_all(path.join("agents")).expect("scratch");
        Self(path)
    }

    fn settings(&self) -> AgentSettings {
        AgentSettings::new(self.0.join("agents"))
    }

    fn write(&self, agent: &str, text: &str) {
        std::fs::write(self.0.join("agents").join(format!("{agent}.toml")), text).expect("write");
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            eprintln!("couldn't remove {}: {e}", self.0.display());
        }
    }
}

fn grok() -> AgentId {
    AgentId::from("grok")
}

fn claude() -> AgentId {
    AgentId::from("claude")
}

fn name(text: &str) -> AgentName {
    AgentName::new(text).expect("plain")
}

// ---- TP-ANM-02: the saved name ---------------------------------------------------------------

#[test]
fn tp_anm_02_a_name_is_saved_beside_the_model_and_cap() {
    let s = Scratch::new();
    let setup = AgentSetup {
        model: model_choice(Provider::Anthropic, "claude-haiku-5-5"),
        monthly_cap_micro_usd: Some(5_000_000),
    };
    s.settings().save(&claude(), &setup).expect("saved");
    s.settings()
        .save_name(&claude(), Some(&name("Ada")))
        .expect("named");
    assert_eq!(s.settings().name(&claude()), Ok(Some(name("Ada"))));
    assert_eq!(
        s.settings().load(&claude()),
        Ok(setup),
        "the model and cap stay"
    );
    s.settings().save(&claude(), &setup).expect("saved again");
    assert_eq!(
        s.settings().name(&claude()),
        Ok(Some(name("Ada"))),
        "and saving them keeps the name"
    );
    s.settings().save_name(&claude(), None).expect("back");
    assert_eq!(s.settings().name(&claude()), Ok(None));
    assert_eq!(s.settings().name(&grok()), Ok(None), "no file, no name");
}

#[test]
fn tp_anm_02_a_saved_name_that_cant_be_used_is_an_error_naming_the_file() {
    for (text, why) in [
        ("name = \"a/b\"\n", "'/'"),
        ("name = \"\"\n", "empty"),
        ("name = \"GROK\"\n", "Grok's name"),
        ("name = \"Muse Spark\"\n", "Muse Spark's name"),
    ] {
        let s = Scratch::new();
        s.write("claude", text);
        let error = s.settings().name(&claude()).expect_err(why);
        assert!(error.contains("claude.toml"), "{why}: {error}");
        assert!(error.contains(why), "{why}: {error}");
    }
    let s = Scratch::new();
    assert!(
        s.settings()
            .save_name(&claude(), Some(&name("grok")))
            .is_err(),
        "another agent's catalog name is never saved"
    );
    s.write("claude", "name = \"Claude\"\n");
    assert_eq!(
        s.settings().name(&claude()),
        Ok(Some(name("Claude"))),
        "its own is fine"
    );
}

// ---- TP-ANM-03: one lookup for every name ----------------------------------------------------

#[test]
fn tp_anm_03_without_renames_every_agent_has_its_catalog_name() {
    let names = AgentNames::default();
    for provider in Provider::ALL {
        let card = agent_for(provider);
        assert_eq!(names.name_of(&card.id), card.name);
        assert_eq!(names.card(provider), card);
    }
    assert_eq!(
        names.name_of(&AgentId::from("linked-x")),
        "linked-x",
        "an unknown ID is itself"
    );
}

#[test]
fn tp_anm_03_a_saved_name_renames_the_card_and_the_folder() {
    let s = Scratch::new();
    s.write("claude", "name = \"Ada Lovelace\"\n");
    let (names, problems) = AgentNames::load(&s.settings());
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(names.name_of(&claude()), "Ada Lovelace");
    let card = names.card(Provider::Anthropic);
    assert_eq!(
        (card.id.as_str(), card.name.as_str(), card.monogram.as_str()),
        ("claude", "Ada Lovelace", "AL")
    );
    assert_eq!(
        names.card(Provider::Xai),
        agent_for(Provider::Xai),
        "the others don't change"
    );
    assert_eq!(
        names.folder(Path::new("/Users/anton"), &claude()),
        Path::new("/Users/anton/Corral/Ada Lovelace")
    );
    let linked = AgentCard {
        id: AgentId::from("linked-notes"),
        name: "Notes".into(),
        monogram: "N".into(),
    };
    let renamed = names.rename(vec![
        agent_for(Provider::Xai),
        agent_for(Provider::Anthropic),
        linked.clone(),
    ]);
    let shown: Vec<&str> = renamed.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(
        shown,
        ["Grok", "Ada Lovelace", "Notes"],
        "a linked app keeps its own name"
    );
    assert_eq!(renamed[2], linked);
}

#[test]
fn tp_anm_03_a_name_that_cant_be_used_leaves_the_catalog_name_and_says_why() {
    let s = Scratch::new();
    s.write("claude", "name = \"../x\"\n");
    s.write("grok", "name = \"Ada\"\n");
    s.write("gpt", "name = \"ada\"\n");
    s.write("muse-spark", "name = \"Iris\"\n");
    let (names, problems) = AgentNames::load(&s.settings());
    assert_eq!(names.name_of(&claude()), "Claude");
    assert_eq!(
        names.name_of(&grok()),
        "Grok",
        "two agents can't share a name"
    );
    assert_eq!(names.name_of(&AgentId::from("gpt")), "GPT");
    assert_eq!(names.name_of(&AgentId::from("muse-spark")), "Iris");
    assert_eq!(
        names.folder(Path::new("/h"), &grok()),
        Path::new("/h/Corral/Grok"),
        "its own folder"
    );
    assert_eq!(problems.len(), 2, "one each: {problems:?}");
    assert!(
        problems.iter().any(|p| p.contains("claude.toml")),
        "{problems:?}"
    );
    assert!(
        problems
            .iter()
            .any(|p| p.contains("Grok and GPT") && p.contains("\"Ada\"")),
        "{problems:?}"
    );
}

#[test]
fn tp_anm_03_a_rename_reaches_everyone_holding_the_lookup() {
    let names = AgentNames::default();
    let held = names.clone();
    names.set(&claude(), Some(name("Ada"))).expect("renamed");
    assert_eq!(held.name_of(&claude()), "Ada");
    names.set(&claude(), None).expect("back");
    assert_eq!(held.name_of(&claude()), "Claude");
}

#[test]
fn tp_anm_03_a_rename_cant_take_another_agents_name() {
    let names = AgentNames::default();
    names.set(&claude(), Some(name("Ada"))).expect("renamed");
    let gpt = AgentId::from("gpt");
    let taken = names
        .set(&gpt, Some(name("ADA")))
        .expect_err("Claude's now");
    assert!(
        taken.contains("taken by Ada"),
        "named as it is now: {taken}"
    );
    let catalog = names.set(&gpt, Some(name("grok"))).expect_err("Grok's");
    assert!(catalog.contains("Grok"), "{catalog}");
    assert_eq!(
        names.name_of(&gpt),
        "GPT",
        "a refused rename changes nothing"
    );
    names
        .set(&claude(), Some(name("ada")))
        .expect("its own name, recased");
    assert_eq!(names.name_of(&claude()), "ada");
    names
        .set(&claude(), Some(name("Claude")))
        .expect("its own catalog name");
    let linked = AgentId::from("linked-notes");
    assert!(
        names.set(&linked, Some(name("Iris"))).is_err(),
        "only Corral's agents"
    );
    let card = AgentCard {
        id: linked,
        name: "Notes".into(),
        monogram: "N".into(),
    };
    assert_eq!(names.rename(vec![card.clone()]), [card]);
}

// ---- TP-ANM-04: the main window's sections show the name -------------------------------------

fn renamed() -> AgentNames {
    let names = AgentNames::default();
    names.set(&claude(), Some(name("Ada"))).expect("renamed");
    names
}

#[test]
fn tp_anm_04_the_policy_editor_and_provider_tools_show_it() {
    let s = Scratch::new();
    let editor = PolicyEditor::open(s.0.join("policies")).named(renamed());
    let views = editor.views();
    let view = views.iter().find(|v| v.agent == "claude").expect("claude");
    assert_eq!(view.agent_name, "Ada");
    let tools = ProviderTools::open(s.0.join("provider-tools.json")).named(renamed());
    let choice = tools
        .view()
        .choices
        .into_iter()
        .find(|c| c.agent == "claude")
        .expect("claude's search");
    assert_eq!(choice.agent_name, "Ada");
}

#[test]
fn tp_anm_04_the_trace_list_shows_it() {
    let s = Scratch::new();
    let store = Arc::new(Store::open(&s.0.join("corral.db")).expect("open"));
    store
        .append(&Event {
            run_id: RunId::from("r1"),
            seq: 1,
            ts: 1_000,
            span_id: SpanId::from("r1:invoke_agent"),
            parent_span_id: None,
            body: EventBody::RunQueued(RunQueued {
                agent_id: claude(),
                session_id: SessionId::from("s1"),
                prompt: "Plan my week".into(),
            }),
        })
        .expect("append");
    let traces = Traces::named(renamed());
    traces.attach(store);
    let runs = traces.recent_runs().expect("list");
    assert_eq!(runs[0].agent_name, "Ada");
}

//! Each agent's settings, saved (M4 item 6; TP-AED-03..04): `<app data>/agents/<agent>.toml`
//! holds the model it uses and its own monthly cap. No file means the default model and the
//! default cap. A file that's there but can't be used is an error naming the file, and at launch
//! that agent's monthly cap is $0, so a broken file never quietly raises a cap the user lowered
//! (AGENTS.md: checks fail closed). A cap above the all-agents cap is refused the same way.

use corral_adapters::catalog::{CLAUDE_MODEL, GROK_MODEL, ModelChoice, model_choice};
use corral_core::budget::{LedgerConfig, LocalMonths, Price, SpendLedger};
use corral_core::ids::AgentId;
use corral_desktop_lib::agent_settings::{
    AgentSettings, AgentSetup, MAX_SETTINGS_FILE_BYTES, messages_config, responses_config,
    seed_caps,
};
use corral_secrets::Provider;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

/// A fresh scratch folder, cleaned up on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "corral-agent-settings-{}-{}",
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

fn ledger() -> SpendLedger {
    SpendLedger::new(LedgerConfig::default(), LocalMonths::fixed(0).expect("utc"))
}

#[test]
fn tp_aed_03_no_file_means_the_default_model_and_cap() {
    let s = Scratch::new();
    let setup = s.settings().load(&claude()).expect("defaults");
    assert_eq!(setup.model.map(|m| m.id), Some(CLAUDE_MODEL));
    assert_eq!(setup.monthly_cap_micro_usd, None);
    let gpt = s.settings().load(&AgentId::from("gpt")).expect("defaults");
    assert_eq!(gpt.model, None, "no client yet, so no model");
}

#[test]
fn tp_aed_03_a_saved_setup_loads_back_the_same() {
    let s = Scratch::new();
    let setup = AgentSetup {
        model: model_choice(Provider::Anthropic, "claude-haiku-5-5"),
        monthly_cap_micro_usd: Some(10_000_000),
    };
    s.settings().save(&claude(), &setup).expect("saved");
    assert_eq!(s.settings().load(&claude()), Ok(setup));
    let text = std::fs::read_to_string(s.0.join("agents/claude.toml")).expect("read");
    assert!(text.contains("model = \"claude-haiku-5-5\""), "{text}");
    assert!(
        !s.0.join("agents/claude.toml.tmp").exists(),
        "renamed into place"
    );
    assert_eq!(
        s.settings().load(&grok()).map(|g| g.model.map(|m| m.id)),
        Ok(Some(GROK_MODEL))
    );
}

#[test]
fn tp_aed_03_a_file_that_cant_be_used_is_an_error_naming_it() {
    for (text, why) in [
        (
            "model = \"grok-4.7\"\n",
            "isn't a model Corral offers for Claude",
        ),
        (
            "model = \"claude-3-opus\"\n",
            "isn't a model Corral offers for Claude",
        ),
        ("colour = \"blue\"\n", "colour"),
        ("monthly_cap_micro_usd = -5\n", "monthly_cap_micro_usd"),
        ("model = \n", "claude.toml"),
    ] {
        let s = Scratch::new();
        s.write("claude", text);
        let error = s.settings().load(&claude()).expect_err(why);
        assert!(error.contains("claude.toml"), "{why}: {error}");
        assert!(error.contains(why), "{why}: {error}");
    }
    assert_eq!(MAX_SETTINGS_FILE_BYTES, 4 * 1024);
    let s = Scratch::new();
    s.write("claude", &"#".repeat(MAX_SETTINGS_FILE_BYTES + 1));
    assert!(
        s.settings()
            .load(&claude())
            .expect_err("too big")
            .contains("4 KB")
    );
    s.write(
        "claude",
        &format!("{}\n", "#".repeat(MAX_SETTINGS_FILE_BYTES - 1)),
    );
    assert!(
        s.settings().load(&claude()).is_ok(),
        "exactly the most is fine"
    );
}

#[test]
fn tp_aed_03_a_file_that_cant_be_read_is_an_error_not_the_defaults() {
    let s = Scratch::new();
    std::fs::create_dir_all(s.0.join("agents/claude.toml")).expect("a folder in its place");
    let error = s.settings().load(&claude()).expect_err("unreadable");
    assert!(error.contains("claude.toml"), "{error}");
}

#[test]
fn tp_aed_03_only_plain_agent_ids_are_kept() {
    let s = Scratch::new();
    for id in ["../claude", "", "a/b", "claude.toml"] {
        assert!(s.settings().load(&AgentId::from(id)).is_err(), "{id:?}");
    }
    assert!(s.settings().load(&AgentId::from("muse-spark")).is_ok());
}

#[test]
fn tp_aed_04_launch_sets_each_agents_cap_and_a_broken_file_caps_it_at_zero() {
    let s = Scratch::new();
    s.write("claude", "monthly_cap_micro_usd = 5000000\n");
    s.write("grok", "not toml at all\n");
    let ledger = ledger();
    let problems = seed_caps(
        &ledger,
        &s.settings(),
        &[grok(), claude(), AgentId::from("gpt")],
    );
    assert_eq!(ledger.agent_cap(&claude()), 5_000_000);
    assert_eq!(ledger.agent_cap(&grok()), 0, "fails closed");
    assert_eq!(
        ledger.agent_cap(&AgentId::from("gpt")),
        25_000_000,
        "the default"
    );
    assert_eq!(problems.len(), 1);
    assert!(problems[0].contains("grok.toml"), "{}", problems[0]);
    assert!(problems[0].contains("Grok can't spend"), "{}", problems[0]);
}

#[test]
fn tp_aed_04_a_cap_above_the_all_agents_cap_is_refused_at_launch() {
    let s = Scratch::new();
    s.write("claude", "monthly_cap_micro_usd = 100000001\n");
    let ledger = ledger();
    let problems = seed_caps(&ledger, &s.settings(), &[claude()]);
    assert_eq!(ledger.agent_cap(&claude()), 0);
    assert!(problems[0].contains("$100.00"), "{}", problems[0]);
}

#[test]
fn tp_aed_04_the_clients_use_the_chosen_model_at_its_price() {
    let haiku = model_choice(Provider::Anthropic, "claude-haiku-5-5").expect("offered");
    let config = messages_config(haiku);
    assert_eq!(config.model, "claude-haiku-5-5");
    assert_eq!(config.price, haiku.price);
    let grok = model_choice(Provider::Xai, GROK_MODEL).expect("offered");
    let config = responses_config(grok);
    assert_eq!(config.model, GROK_MODEL);
    assert_eq!(config.price, grok.price);
    assert_eq!(config.base_url, "https://api.x.ai/v1");
    let other = ModelChoice {
        id: "grok-other",
        name: "Grok Other",
        price: Price::per_million_tokens(1, 1),
    };
    assert_eq!(
        responses_config(&other).price,
        other.price,
        "its own price, not Grok 4.7's"
    );
    assert_eq!(messages_config(&other).price, other.price);
}

// ---- TP-SUB-08: GPT's model on the ChatGPT plan ---------------------------------------------

#[test]
fn tp_sub_08_gpts_saved_model_is_its_plan_model() {
    let s = Scratch::new();
    let gpt = AgentId::from("gpt");
    assert_eq!(s.settings().plan_model(&gpt).expect("no file"), None);
    s.write("gpt", "model = \"gpt-alpha\"\nmonthly_cap_micro_usd = 0\n");
    let setup = s
        .settings()
        .load(&gpt)
        .expect("a plan model is GPT's to have");
    assert_eq!(setup.model, None, "not one of the priced table's");
    assert_eq!(setup.monthly_cap_micro_usd, Some(0));
    assert_eq!(
        s.settings().plan_model(&gpt).expect("readable").as_deref(),
        Some("gpt-alpha")
    );
    // A model that couldn't have come from the plan's list fails closed, naming the file.
    s.write("gpt", "model = \"gpt alpha; rm\"\n");
    let why = s.settings().load(&gpt).expect_err("not a slug");
    assert!(why.contains("gpt.toml"), "{why}");
    assert!(s.settings().plan_model(&gpt).is_err());
    // Only an agent that runs on the plan has one.
    s.write("claude", "model = \"gpt-alpha\"\n");
    assert!(s.settings().load(&claude()).is_err());
    assert!(s.settings().plan_model(&claude()).is_err());
}

#[test]
fn tp_sub_08_saving_a_plan_model_keeps_the_name_and_cap() {
    let s = Scratch::new();
    let gpt = AgentId::from("gpt");
    s.write("gpt", "name = \"Ada\"\nmonthly_cap_micro_usd = 1000000\n");
    let settings = s.settings();
    let setup = settings.load(&gpt).expect("defaults");
    let name = settings.name(&gpt).expect("a name");
    settings
        .save_all_on_plan(&gpt, &setup, Some("gpt-beta"), name.as_ref())
        .expect("saved");
    assert_eq!(
        settings.plan_model(&gpt).expect("readable").as_deref(),
        Some("gpt-beta")
    );
    assert_eq!(
        settings
            .name(&gpt)
            .expect("a name")
            .map(|n| n.as_str().to_owned()),
        Some("Ada".into())
    );
    assert_eq!(
        settings.load(&gpt).expect("usable").monthly_cap_micro_usd,
        Some(1_000_000)
    );
    // Saving the cap the old way keeps the plan model.
    settings
        .save(
            &gpt,
            &AgentSetup {
                model: None,
                monthly_cap_micro_usd: Some(2_000_000),
            },
        )
        .expect("saved");
    assert_eq!(
        settings.plan_model(&gpt).expect("readable").as_deref(),
        Some("gpt-beta")
    );
    // None goes back to the plan's first; a slug that isn't one, or a key-billed agent, is refused.
    settings
        .save_all_on_plan(&gpt, &setup, None, None)
        .expect("saved");
    assert_eq!(settings.plan_model(&gpt).expect("readable"), None);
    assert!(
        settings
            .save_all_on_plan(&gpt, &setup, Some("a b"), None)
            .is_err()
    );
    assert!(
        settings
            .save_all_on_plan(&claude(), &setup, Some("gpt-beta"), None)
            .is_err()
    );
}

//! The agent editor's core (M4 item 6; TP-AED-05..08): for each agent, its name and folder, its
//! model from the priced table, its own monthly cap, and what it spent this month. A save is
//! checked whole before anything is written: a refused one says why and changes nothing. A
//! saved one is one write, then applies without a restart: the cap from the agent's next
//! request, the name on its card at once, and the model and folder from its next task.

use corral_adapters::catalog::{CLAUDE_MODEL, GROK_MODEL};
use corral_core::agent_name::AgentName;
use corral_core::budget::Price;
use corral_core::budget::{LedgerConfig, LocalMonths, SpendLedger};
use corral_core::clock::ManualClock;
use corral_core::ids::AgentId;
use corral_desktop_lib::agent_editor::{AgentEdit, AgentEditor, AgentEditorView, price_text};
use corral_desktop_lib::agent_names::AgentNames;
use corral_desktop_lib::agent_settings::AgentSettings;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// 2026-11-15 12:00 UTC, in ms.
const NOW: u64 = 1_794_744_000_000;

/// A scratch app data folder and home, cleaned up on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static N: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "corral-agent-editor-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        if path.exists() {
            std::fs::remove_dir_all(&path).expect("clear");
        }
        std::fs::create_dir_all(path.join("data/agents")).expect("data");
        std::fs::create_dir_all(path.join("home/Corral")).expect("home");
        Self(path)
    }

    fn settings(&self) -> AgentSettings {
        AgentSettings::new(self.0.join("data/agents"))
    }

    fn home(&self) -> PathBuf {
        self.0.join("home")
    }

    fn file(&self, agent: &str) -> PathBuf {
        self.0.join(format!("data/agents/{agent}.toml"))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            eprintln!("couldn't remove {}: {e}", self.0.display());
        }
    }
}

/// An editor over `s`, with a ledger, and a count of how often the corral was told to refresh.
struct Rig {
    editor: AgentEditor,
    names: AgentNames,
    ledger: Arc<SpendLedger>,
    refreshed: Arc<AtomicUsize>,
}

fn rig(s: &Scratch) -> Rig {
    let names = AgentNames::default();
    let editor = AgentEditor::open(s.settings(), names.clone(), Some(s.home()));
    let ledger = Arc::new(SpendLedger::new(
        LedgerConfig::default(),
        LocalMonths::fixed(0).expect("utc"),
    ));
    editor.attach(Arc::clone(&ledger), Arc::new(ManualClock::at(NOW)));
    let refreshed = Arc::new(AtomicUsize::new(0));
    let count = Arc::clone(&refreshed);
    editor.on_change(Box::new(move || {
        count.fetch_add(1, Ordering::SeqCst);
    }));
    Rig {
        editor,
        names,
        ledger,
        refreshed,
    }
}

fn view<'a>(views: &'a [AgentEditorView], agent: &str) -> &'a AgentEditorView {
    views.iter().find(|v| v.agent == agent).expect("listed")
}

fn edit(name: Option<&str>, model: Option<&str>, cap: Option<u64>) -> AgentEdit {
    AgentEdit {
        name: name.map(str::to_owned),
        model: model.map(str::to_owned),
        monthly_cap_micro_usd: cap,
    }
}

fn claude() -> AgentId {
    AgentId::from("claude")
}

// ---- TP-AED-05: what the editor shows --------------------------------------------------------

#[test]
fn tp_aed_05_every_agent_with_its_name_folder_model_and_cap() {
    let s = Scratch::new();
    let r = rig(&s);
    r.ledger.seed(&claude(), NOW, 1_250_000);
    let views = r.editor.views();
    let ids: Vec<&str> = views.iter().map(|v| v.agent.as_str()).collect();
    assert_eq!(
        ids,
        ["grok", "gpt", "claude", "muse-spark"],
        "the catalog's order"
    );
    let c = view(&views, "claude");
    assert_eq!(
        (c.name.as_str(), c.catalog_name.as_str()),
        ("Claude", "Claude")
    );
    assert_eq!(c.folder, "~/Corral/Claude");
    assert_eq!(c.model.as_deref(), Some(CLAUDE_MODEL));
    let models: Vec<&str> = c.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        models,
        [CLAUDE_MODEL, "claude-opus-5-5", "claude-haiku-5-5"]
    );
    let haiku = c
        .models
        .iter()
        .find(|m| m.id == "claude-haiku-5-5")
        .expect("haiku");
    assert_eq!(haiku.name, "Claude Haiku 5.5");
    assert_eq!(
        haiku.price,
        "$0.10 in and $0.50 out per million tokens; $0.50 and $2.50 from 100,001 prompt tokens"
    );
    assert_eq!(c.monthly_cap_micro_usd, None);
    assert_eq!(c.default_cap_micro_usd, 25_000_000);
    assert_eq!(c.max_cap_micro_usd, 100_000_000);
    assert_eq!(c.spent_micro_usd, Some(1_250_000));
    assert_eq!(c.problem, None);
    let grok = view(&views, "grok");
    assert_eq!(
        grok.models[0].price,
        "$2.00 in and $6.00 out per million tokens; $4.00 and $12.00 from 200,000 prompt tokens"
    );
    let gpt = view(&views, "gpt");
    assert!(gpt.models.is_empty());
    assert_eq!(gpt.model, None, "no client yet");
}

#[test]
fn tp_aed_05_without_a_ledger_there_is_no_spend_to_show() {
    let s = Scratch::new();
    let editor = AgentEditor::open(s.settings(), AgentNames::default(), None);
    let views = editor.views();
    assert_eq!(view(&views, "grok").spent_micro_usd, None);
    assert_eq!(view(&views, "grok").folder, "~/Corral/Grok");
    let refused = editor
        .save("grok", edit(Some("Ada"), None, None))
        .expect_err("no home folder to check");
    assert!(refused.contains("can't find your home folder"), "{refused}");
    editor
        .save("grok", edit(None, None, Some(1_000_000)))
        .expect("everything else still saves");
}

#[test]
fn tp_aed_05_settings_that_cant_be_used_are_shown_as_a_problem() {
    let s = Scratch::new();
    std::fs::write(s.file("grok"), "not toml\n").expect("write");
    let r = rig(&s);
    let views = r.editor.views();
    let grok = view(&views, "grok");
    let problem = grok.problem.as_deref().expect("a problem");
    assert!(problem.contains("Grok can't spend"), "{problem}");
    assert!(problem.contains("Saving here replaces"), "{problem}");
    assert_eq!(
        grok.model.as_deref(),
        Some(GROK_MODEL),
        "the defaults are shown"
    );
}

#[test]
fn tp_aed_05_prices_read_with_thousands_separators() {
    let price = |from| {
        price_text(
            &Price::per_million_tokens(1_000_000, 2_000_000)
                .with_long_prompt(from, 3_000_000, 4_000_000),
        )
    };
    assert!(
        price(1_234_567).ends_with("from 1,234,567 prompt tokens"),
        "{}",
        price(1_234_567)
    );
    assert!(price(12_345).ends_with("from 12,345 prompt tokens"));
    assert!(price(999).ends_with("from 999 prompt tokens"));
    assert_eq!(
        price_text(&Price::per_million_tokens(1_000_000, 2_000_000)),
        "$1.00 in and $2.00 out per million tokens",
        "no long rate, nothing more"
    );
}

#[test]
fn tp_aed_05_the_caps_are_the_running_ledgers() {
    let s = Scratch::new();
    let editor = AgentEditor::open(s.settings(), AgentNames::default(), Some(s.home()));
    let ledger = Arc::new(SpendLedger::new(
        LedgerConfig {
            agent_month_cap_micro_usd: 10_000_000,
            global_month_cap_micro_usd: 50_000_000,
        },
        LocalMonths::fixed(0).expect("utc"),
    ));
    editor.attach(Arc::clone(&ledger), Arc::new(ManualClock::at(NOW)));
    let grok = view(&editor.views(), "grok").clone();
    assert_eq!(
        (grok.default_cap_micro_usd, grok.max_cap_micro_usd),
        (10_000_000, 50_000_000)
    );
    let refused = editor
        .save("grok", edit(None, None, Some(50_000_001)))
        .expect_err("over the all-agents cap");
    assert!(refused.contains("$50.00"), "{refused}");
    editor
        .save("grok", edit(None, None, Some(50_000_000)))
        .expect("exactly the all-agents cap");
    assert_eq!(ledger.agent_cap(&AgentId::from("grok")), 50_000_000);
}

#[test]
fn tp_aed_07_a_folder_that_cant_be_checked_refuses_the_rename() {
    let s = Scratch::new();
    std::fs::remove_dir_all(s.home().join("Corral")).expect("clear");
    std::fs::write(s.home().join("Corral"), "a file where the folder goes").expect("write");
    let r = rig(&s);
    let refused = r
        .editor
        .save("claude", edit(Some("Ada"), None, None))
        .expect_err("can't check");
    assert!(refused.contains("Couldn't check ~/Corral/Ada"), "{refused}");
    assert_eq!(r.names.name_of(&claude()), "Claude");
}

// ---- TP-AED-06: a save, applied --------------------------------------------------------------

#[test]
fn tp_aed_06_a_save_is_kept_and_applies_without_a_restart() {
    let s = Scratch::new();
    let r = rig(&s);
    let before = r.editor.version(&claude());
    let saved = r
        .editor
        .save(
            "claude",
            edit(Some(" Ada "), Some("claude-haiku-5-5"), Some(5_000_000)),
        )
        .expect("saved");
    assert_eq!(saved.name, "Ada");
    assert_eq!(saved.folder, "~/Corral/Ada");
    assert_eq!(saved.model.as_deref(), Some("claude-haiku-5-5"));
    assert_eq!(saved.monthly_cap_micro_usd, Some(5_000_000));
    assert_eq!(r.ledger.agent_cap(&claude()), 5_000_000, "the next request");
    assert_eq!(r.names.name_of(&claude()), "Ada", "the card");
    assert_eq!(r.refreshed.load(Ordering::SeqCst), 1, "the corral was told");
    assert_eq!(r.editor.version(&claude()), before + 1, "the next task");
    assert_eq!(
        r.editor.version(&AgentId::from("grok")),
        0,
        "only that agent"
    );
    let settings = s.settings();
    assert_eq!(
        settings.name(&claude()),
        Ok(Some(AgentName::new("Ada").expect("plain")))
    );
    let setup = settings.load(&claude()).expect("kept");
    assert_eq!(setup.model.map(|m| m.id), Some("claude-haiku-5-5"));
    assert_eq!(setup.monthly_cap_micro_usd, Some(5_000_000));
}

#[test]
fn tp_aed_06_an_empty_name_and_no_cap_go_back_to_the_defaults() {
    let s = Scratch::new();
    let r = rig(&s);
    r.editor
        .save("claude", edit(Some("Ada"), None, Some(1_000_000)))
        .expect("saved");
    let back = r
        .editor
        .save("claude", edit(Some("  "), None, None))
        .expect("saved");
    assert_eq!(back.name, "Claude");
    assert_eq!(
        back.model.as_deref(),
        Some(CLAUDE_MODEL),
        "no model is the default"
    );
    assert_eq!(back.monthly_cap_micro_usd, None);
    assert_eq!(r.ledger.agent_cap(&claude()), 25_000_000);
    assert_eq!(
        s.settings().name(&claude()),
        Ok(None),
        "its catalog name isn't saved"
    );
    let named = r
        .editor
        .save("claude", edit(Some("claude"), None, None))
        .expect("saved");
    assert_eq!(
        named.name, "Claude",
        "its own catalog name, in any case, is no rename"
    );
}

#[test]
fn tp_aed_06_an_agent_can_go_back_to_its_own_folder_and_recase_its_name() {
    let s = Scratch::new();
    std::fs::create_dir_all(s.home().join("Corral/Claude")).expect("its folder");
    let r = rig(&s);
    r.editor
        .save("claude", edit(Some("Ada"), None, None))
        .expect("renamed");
    std::fs::create_dir_all(s.home().join("Corral/Ada")).expect("made by its first task");
    r.editor
        .save("claude", edit(Some("ADA"), None, None))
        .expect("the same folder");
    r.editor
        .save("claude", edit(None, None, None))
        .expect("back to Claude's own");
    assert_eq!(r.names.name_of(&claude()), "Claude");
}

// ---- TP-AED-07: a refused save changes nothing -----------------------------------------------

#[test]
fn tp_aed_07_a_refused_save_says_why_and_changes_nothing() {
    let s = Scratch::new();
    std::fs::create_dir_all(s.home().join("Corral/Iris")).expect("someone's folder");
    let r = rig(&s);
    r.editor
        .save("grok", edit(Some("Ada"), None, None))
        .expect("Grok is Ada");
    let refreshed = r.refreshed.load(Ordering::SeqCst);
    let start = view(&r.editor.views(), "claude").clone();
    for (agent, change, why) in [
        ("claude", edit(Some("a/b"), None, None), "'/'"),
        ("claude", edit(Some("GPT"), None, None), "GPT"),
        ("claude", edit(Some("ada"), None, None), "taken by Ada"),
        (
            "claude",
            edit(Some("Iris"), None, None),
            "~/Corral/Iris already exists",
        ),
        (
            "claude",
            edit(None, Some(GROK_MODEL), None),
            "isn't a model Corral offers for Claude",
        ),
        ("claude", edit(None, None, Some(100_000_001)), "$100.00"),
        (
            "gpt",
            edit(None, Some("gpt-6"), None),
            "isn't a model Corral offers",
        ),
        (
            "nobody",
            edit(None, None, None),
            "isn't one of Corral's agents",
        ),
    ] {
        let error = r.editor.save(agent, change).expect_err(why);
        assert!(error.contains(why), "{why}: {error}");
    }
    assert_eq!(view(&r.editor.views(), "claude"), &start);
    assert_eq!(r.editor.version(&claude()), 0);
    assert_eq!(r.refreshed.load(Ordering::SeqCst), refreshed);
    assert_eq!(r.ledger.agent_cap(&claude()), 25_000_000);
    assert!(!s.file("claude").exists(), "nothing written");
}

// ---- TP-AED-08: settings that couldn't be used are replaced by a save ------------------------

#[test]
fn tp_aed_08_a_save_replaces_settings_that_couldnt_be_used() {
    let s = Scratch::new();
    std::fs::write(s.file("grok"), "not toml\n").expect("write");
    let r = rig(&s);
    r.ledger
        .set_agent_cap(&AgentId::from("grok"), Some(0))
        .expect("as launch leaves it");
    let saved = r
        .editor
        .save("grok", edit(None, None, Some(2_000_000)))
        .expect("replaced");
    assert_eq!(saved.problem, None);
    assert_eq!(r.ledger.agent_cap(&AgentId::from("grok")), 2_000_000);
    assert!(s.settings().load(&AgentId::from("grok")).is_ok());
}

// ---- TP-SUB-08: GPT's model on the ChatGPT plan ---------------------------------------------

use corral_adapters::chatgpt::PlanModel;
use corral_desktop_lib::agent_editor::{PlanModels, PlanState};

fn plan_models() -> PlanModels {
    PlanModels::Listed(vec![
        PlanModel {
            slug: "gpt-alpha".into(),
            display_name: "GPT Alpha".into(),
        },
        PlanModel {
            slug: "gpt-beta".into(),
            display_name: "GPT Beta".into(),
        },
    ])
}

#[test]
fn tp_sub_08_gpt_offers_the_plans_models_in_place_of_prices() {
    let s = Scratch::new();
    let r = rig(&s);
    let views = r.editor.views_on_plan(&plan_models());
    let gpt = view(&views, "gpt");
    assert_eq!(gpt.chatgpt_plan, Some(PlanState::SignedIn));
    let ids: Vec<&str> = gpt.models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["gpt-alpha", "gpt-beta"], "the plan's order");
    assert_eq!(gpt.models[1].name, "GPT Beta");
    assert!(
        gpt.models
            .iter()
            .all(|m| m.price == "Uses your ChatGPT plan")
    );
    assert_eq!(
        gpt.model.as_deref(),
        Some("gpt-alpha"),
        "the plan's first by default"
    );
    assert_eq!(
        view(&views, "claude").chatgpt_plan,
        None,
        "Claude runs on its key"
    );

    let signed_out = r.editor.views();
    let gpt = view(&signed_out, "gpt");
    assert_eq!(gpt.chatgpt_plan, Some(PlanState::SignedOut));
    assert!(gpt.models.is_empty());
    let down = r
        .editor
        .views_on_plan(&PlanModels::Unavailable("Couldn't reach ChatGPT.".into()));
    assert_eq!(
        view(&down, "gpt").chatgpt_plan,
        Some(PlanState::Unavailable {
            message: "Couldn't reach ChatGPT.".into()
        })
    );
}

#[test]
fn tp_sub_08_a_saved_plan_model_shows_while_the_plan_lists_it() {
    let s = Scratch::new();
    let r = rig(&s);
    let kept = r
        .editor
        .save_on_plan("gpt", edit(None, Some("gpt-beta"), None), &plan_models())
        .expect("a listed model");
    assert_eq!(kept.model.as_deref(), Some("gpt-beta"));
    assert_eq!(
        r.editor.version(&AgentId::from("gpt")),
        1,
        "its next task uses it"
    );
    assert_eq!(
        s.settings()
            .plan_model(&AgentId::from("gpt"))
            .expect("saved")
            .as_deref(),
        Some("gpt-beta")
    );
    // The plan stops listing it: the editor shows the plan's first, as the run would use.
    let fewer = PlanModels::Listed(vec![PlanModel {
        slug: "gpt-alpha".into(),
        display_name: "GPT Alpha".into(),
    }]);
    let views = r.editor.views_on_plan(&fewer);
    assert_eq!(view(&views, "gpt").model.as_deref(), Some("gpt-alpha"));
}

#[test]
fn tp_sub_08_a_model_the_plan_doesnt_list_is_refused_and_nothing_changes() {
    let s = Scratch::new();
    let r = rig(&s);
    for (models, why) in [
        (plan_models(), "isn't a model your ChatGPT plan lists"),
        (PlanModels::SignedOut, "Continue with ChatGPT"),
        (
            PlanModels::Unavailable("Couldn't reach ChatGPT.".into()),
            "Couldn't reach ChatGPT.",
        ),
    ] {
        let error = r
            .editor
            .save_on_plan("gpt", edit(Some("Ada"), Some("gpt-retired"), None), &models)
            .expect_err(why);
        assert!(error.contains(why), "{why}: {error}");
    }
    assert!(!s.file("gpt").exists(), "nothing written");
    assert_eq!(r.editor.version(&AgentId::from("gpt")), 0);
    // Claude's models stay the priced table's, whatever the plan lists.
    let error = r
        .editor
        .save_on_plan(
            "claude",
            edit(None, Some("gpt-alpha"), None),
            &plan_models(),
        )
        .expect_err("not Claude's");
    assert!(
        error.contains("isn't a model Corral offers for Claude"),
        "{error}"
    );
}

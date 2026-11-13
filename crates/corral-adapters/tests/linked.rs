//! TP-LINK-01..02: linked apps (ADR-005). Consumer Muse and your dot can't be driven by Corral (no
//! official API), so they appear as linked drops: a picture and an Open button, never a task
//! field. Which ones show, and which look your dot has, is the user's choice.

use corral_adapters::catalog::{agent_for, corral_agents};
use corral_adapters::linked::{DotLook, LinkChange, LinkedApp, LinkedSettings};
use corral_adapters::logos::{LogoId, logo_source, redirect_allowed};
use corral_core::ids::AgentId;
use corral_secrets::Provider;

fn host_of(url: &str) -> &str {
    let rest = url.strip_prefix("https://").expect("https");
    rest.split(['/', ':', '?', '#']).next().expect("a host")
}

fn ids(cards: &[corral_core::corral_view::AgentCard]) -> Vec<&str> {
    cards.iter().map(|c| c.id.as_str()).collect()
}

// ---- TP-LINK-01: what a linked drop is ---------------------------------------------------------

#[test]
fn tp_link_01_linked_apps_have_their_own_drops() {
    assert_eq!(LinkedApp::Muse.agent_card().id, AgentId::from("muse"));
    assert_eq!(LinkedApp::Muse.agent_card().name, "Muse");
    assert_eq!(LinkedApp::Dot.agent_card().id, AgentId::from("dot"));
    assert_eq!(LinkedApp::Dot.agent_card().name, "Your dot");
    for app in LinkedApp::ALL {
        let card = app.agent_card();
        assert_eq!(LinkedApp::from_agent(&card.id), Some(app));
        assert_eq!(LinkedApp::from_id(app.id()), Some(app));
        for provider in Provider::ALL {
            assert_ne!(
                card.id,
                agent_for(provider).id,
                "a linked drop never shares an ID with an agent Corral runs"
            );
        }
    }
    assert_eq!(LinkedApp::from_agent(&AgentId::from("grok")), None);
    assert_eq!(LinkedApp::from_id("muse-spark"), None);
}

#[test]
fn tp_link_01_open_goes_to_the_apps_own_site_over_https() {
    assert_eq!(LinkedApp::Muse.open_url(), "https://muse.ai/");
    assert_eq!(LinkedApp::Muse.app_name(), "Muse");
    assert_eq!(LinkedApp::Dot.open_url(), "https://chatgpt.com/dots");
    assert_eq!(LinkedApp::Dot.app_name(), "ChatGPT");
}

#[test]
fn tp_link_01_linked_drops_follow_the_agents_corral_runs() {
    let none = LinkedSettings::default();
    assert!(none.is_empty());
    assert_eq!(
        ids(&corral_agents(&[Provider::Xai], &none)),
        ["grok"],
        "nothing linked by default"
    );
    let both = LinkedSettings {
        muse: true,
        dot: Some(DotLook::Felipe),
    };
    assert_eq!(
        ids(&corral_agents(&[Provider::Anthropic, Provider::Xai], &both)),
        ["grok", "claude", "muse", "dot"]
    );
    assert_eq!(both.shown(), [LinkedApp::Muse, LinkedApp::Dot]);
    let dot_only = LinkedSettings {
        muse: false,
        dot: Some(DotLook::Ring),
    };
    assert_eq!(ids(&corral_agents(&[], &dot_only)), ["dot"]);
}

// ---- TP-LINK-02: the user's choices -------------------------------------------------------------

#[test]
fn tp_link_02_showing_and_hiding_change_only_what_was_asked() {
    let mut s = LinkedSettings::default();
    assert!(s.apply(LinkChange::Show {
        app: LinkedApp::Muse,
        look: None
    }));
    assert_eq!(
        s,
        LinkedSettings {
            muse: true,
            dot: None
        }
    );
    assert!(
        !s.apply(LinkChange::Show {
            app: LinkedApp::Muse,
            look: None
        }),
        "no change"
    );

    // Your dot without a look starts as the ring; a look changes it; showing again keeps it.
    assert!(s.apply(LinkChange::Show {
        app: LinkedApp::Dot,
        look: None
    }));
    assert_eq!(s.dot, Some(DotLook::Ring));
    assert!(s.apply(LinkChange::Show {
        app: LinkedApp::Dot,
        look: Some(DotLook::Iggy)
    }));
    assert_eq!(s.dot, Some(DotLook::Iggy));
    assert!(!s.apply(LinkChange::Show {
        app: LinkedApp::Dot,
        look: None
    }));
    assert_eq!(s.dot, Some(DotLook::Iggy));

    assert!(s.apply(LinkChange::Hide(LinkedApp::Muse)));
    assert!(s.apply(LinkChange::Hide(LinkedApp::Dot)));
    assert!(s.is_empty());
    assert!(!s.apply(LinkChange::Hide(LinkedApp::Dot)));
}

#[test]
fn tp_link_02_settings_are_saved_as_plain_json() {
    let s = LinkedSettings {
        muse: true,
        dot: Some(DotLook::Todd),
    };
    let json = serde_json::to_value(s).expect("encode");
    assert_eq!(json, serde_json::json!({"muse": true, "dot": "todd"}));
    let back: LinkedSettings = serde_json::from_value(json).expect("decode");
    assert_eq!(back, s);
    let empty: LinkedSettings =
        serde_json::from_str("{}").expect("an empty file is nothing linked");
    assert!(empty.is_empty());
    for bad in [
        r#"{"dot":"nobody"}"#,
        r#"{"muse":"yes"}"#,
        r#"{"muse":true,"extra":1}"#,
    ] {
        assert!(
            serde_json::from_str::<LinkedSettings>(bad).is_err(),
            "{bad}"
        );
    }
}

#[test]
fn tp_link_02_dot_looks_have_ids_and_names() {
    let looks: Vec<(&str, &str)> = DotLook::ALL.iter().map(|l| (l.id(), l.title())).collect();
    assert_eq!(
        looks,
        [
            ("ring", "Ring"),
            ("felipe", "Felipe"),
            ("alfred", "Alfred"),
            ("iggy", "Iggy"),
            ("todd", "Todd"),
        ]
    );
    for look in DotLook::ALL {
        assert_eq!(DotLook::from_id(look.id()), Some(look));
    }
    assert_eq!(DotLook::from_id("Felipe"), None, "IDs are exact");
}

// ---- TP-LOGO-05 for linked apps: their pictures -------------------------------------------------

#[test]
fn tp_logo_05_linked_pictures_come_from_the_apps_makers() {
    let s = LinkedSettings {
        muse: true,
        dot: Some(DotLook::Alfred),
    };
    assert_eq!(s.logo(LinkedApp::Muse), Some(LogoId::Muse));
    assert_eq!(s.logo(LinkedApp::Dot), Some(LogoId::Dot(DotLook::Alfred)));
    assert_eq!(LinkedSettings::default().logo(LinkedApp::Dot), None);

    let muse = logo_source(LogoId::Muse).expect("Muse has a picture");
    assert_eq!(host_of(muse.url), "muse.ai");
    assert!(redirect_allowed(muse, muse.url));

    for look in DotLook::ALL {
        let source = logo_source(LogoId::Dot(look)).expect("every look has a picture");
        // OpenAI's own images, on its image host: pinned to OpenAI's folder there.
        assert_eq!(host_of(source.url), "images.ctfassets.net");
        assert_eq!(source.hosts, ["images.ctfassets.net"]);
        assert_eq!(source.path_prefix, "/8su2tbn87fck/");
        assert!(source.url.ends_with("?fm=png&w=256"), "{}", source.url);
        assert!(redirect_allowed(source, source.url), "{}", source.url);
    }
}

#[test]
fn linked_logo_ids_name_their_files() {
    let mut names: Vec<&str> = DotLook::ALL
        .iter()
        .map(|l| LogoId::Dot(*l).name())
        .collect();
    names.push(LogoId::Muse.name());
    assert_eq!(
        names,
        [
            "dot-ring",
            "dot-felipe",
            "dot-alfred",
            "dot-iggy",
            "dot-todd",
            "muse"
        ]
    );
    for name in names {
        // The file names Swift accepts (TP-LOGO-09).
        assert!(
            (1..=32).contains(&name.len())
                && name
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
            "{name}"
        );
    }
}

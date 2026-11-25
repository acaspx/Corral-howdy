//! An agent's name (M4 item 6; TP-ANM-01). The name is also its folder, `~/Corral/<name>`, the
//! sandbox its file tools work in, so it must be one plain path component that can't be confused
//! with another agent's: ASCII letters and digits, spaces, `-` and `_`, starting and ending with a
//! letter or digit, at most 32 characters. Two names are the same name when they differ only in
//! case, as they would on the Mac's file system.

use corral_core::agent_name::{AgentName, MAX_NAME_CHARS, NameError};

fn check(name: &str) -> Result<String, NameError> {
    AgentName::new(name).map(|n| n.as_str().to_owned())
}

#[test]
fn tp_anm_01_a_plain_name_is_kept_trimmed() {
    assert_eq!(check("Ada"), Ok("Ada".into()));
    assert_eq!(check("  Research Bot 2  "), Ok("Research Bot 2".into()));
    assert_eq!(check("night_owl-7"), Ok("night_owl-7".into()));
    assert_eq!(MAX_NAME_CHARS, 32);
    assert!(check(&"a".repeat(32)).is_ok());
}

#[test]
fn tp_anm_01_a_name_that_isnt_one_plain_folder_is_refused() {
    for (name, why) in [
        ("", NameError::Empty),
        ("   ", NameError::Empty),
        ("a/b", NameError::Character('/')),
        ("..", NameError::Character('.')),
        (".hidden", NameError::Character('.')),
        ("Ada:Lovelace", NameError::Character(':')),
        ("tab\there", NameError::Character('\t')),
        ("Gr\u{f6}k", NameError::Character('\u{f6}')),
        ("\u{202e}koRG", NameError::Character('\u{202e}')),
        ("-dash", NameError::Edge),
        ("under_", NameError::Edge),
    ] {
        assert_eq!(check(name), Err(why.clone()), "{name:?}");
    }
    assert_eq!(check(&"a".repeat(33)), Err(NameError::TooLong));
}

#[test]
fn tp_anm_01_refusals_say_what_a_name_can_be() {
    let text = NameError::Character('/').to_string();
    assert!(text.contains("'/'"), "{text}");
    assert!(
        text.contains("letters A to Z, digits, spaces, - and _"),
        "{text}"
    );
    assert!(NameError::TooLong.to_string().contains("32"));
    assert!(
        NameError::Edge
            .to_string()
            .contains("start and end with a letter or digit")
    );
}

#[test]
fn tp_anm_01_names_that_differ_only_in_case_are_the_same() {
    let ada = AgentName::new("Ada").expect("plain");
    assert!(ada.same_as("ADA"));
    assert!(ada.same_as(" ada "));
    assert!(!ada.same_as("Adah"));
    assert_eq!(ada.to_string(), "Ada", "it prints as itself");
}

#[test]
fn tp_anm_01_the_monogram_comes_from_the_name() {
    let mono = |name: &str| AgentName::new(name).expect("plain").monogram();
    assert_eq!(mono("Ada"), "A");
    assert_eq!(mono("muse spark"), "MS");
    assert_eq!(mono("Research Bot 2"), "RB", "two at most");
    assert_eq!(mono("x-ray_team"), "X");
}

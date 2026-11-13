//! Linked apps (ADR-005): consumer agents Corral can show but not drive. Consumer Muse (Meta)
//! and your dot (OpenAI) have no official API that lets another app give them tasks, and Corral
//! never automates them through scraped sessions or shared passwords. So each appears as a
//! linked drop: its picture and an Open button that goes to the app's own site. No task field,
//! no runs, no spend; the card says it isn't managed by Corral.
//!
//! Which linked apps show, and which of OpenAI's published looks your dot has, is the user's
//! choice ([`LinkedSettings`]). Nothing is linked by default.

use crate::logos::LogoId;
use corral_core::corral_view::AgentCard;
use corral_core::ids::AgentId;
use serde::{Deserialize, Serialize};

/// A consumer agent that lives in its own app.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LinkedApp {
    /// Meta's personal agent, at muse.ai and in the Muse app.
    Muse,
    /// Your always-on agent in ChatGPT.
    Dot,
}

impl LinkedApp {
    /// Every linked app, in the order their drops follow the agents Corral runs.
    pub const ALL: [Self; 2] = [Self::Muse, Self::Dot];

    /// The ID on the wire and of the drop.
    pub fn id(self) -> &'static str {
        match self {
            Self::Muse => "muse",
            Self::Dot => "dot",
        }
    }

    /// The app for a wire ID.
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|app| app.id() == id)
    }

    /// The app whose drop this is.
    pub fn from_agent(agent: &AgentId) -> Option<Self> {
        Self::from_id(agent.as_str())
    }

    /// The drop.
    pub fn agent_card(self) -> AgentCard {
        let (name, monogram) = match self {
            Self::Muse => ("Muse", "Mu"),
            Self::Dot => ("Your dot", "D"),
        };
        AgentCard {
            id: AgentId::from(self.id()),
            name: name.into(),
            monogram: monogram.into(),
        }
    }

    /// Where it lives, for "Open …".
    pub fn app_name(self) -> &'static str {
        match self {
            Self::Muse => "Muse",
            Self::Dot => "ChatGPT",
        }
    }

    /// What Open opens: a fixed page on the app's own site.
    pub fn open_url(self) -> &'static str {
        match self {
            Self::Muse => "https://muse.ai/",
            Self::Dot => "https://chatgpt.com/dots",
        }
    }
}

/// The looks OpenAI publishes for dots (its dots page, November 2026). Corral can't see your own
/// dot, so you pick the one that matches; the ring is OpenAI's plain dots mark.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DotLook {
    /// The dots mark.
    Ring,
    /// Blue, in a beret.
    Felipe,
    /// Yellow, with glasses.
    Alfred,
    /// Pink heart.
    Iggy,
    /// Green frog.
    Todd,
}

impl DotLook {
    /// Every look, in menu order.
    pub const ALL: [Self; 5] = [
        Self::Ring,
        Self::Felipe,
        Self::Alfred,
        Self::Iggy,
        Self::Todd,
    ];

    /// The ID on the wire and in the settings file.
    pub fn id(self) -> &'static str {
        match self {
            Self::Ring => "ring",
            Self::Felipe => "felipe",
            Self::Alfred => "alfred",
            Self::Iggy => "iggy",
            Self::Todd => "todd",
        }
    }

    /// The menu wording.
    pub fn title(self) -> &'static str {
        match self {
            Self::Ring => "Ring",
            Self::Felipe => "Felipe",
            Self::Alfred => "Alfred",
            Self::Iggy => "Iggy",
            Self::Todd => "Todd",
        }
    }

    /// The look for a wire ID (exact).
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|look| look.id() == id)
    }
}

/// Which linked apps show. Saved as `{"muse": true, "dot": "felipe"}`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LinkedSettings {
    /// Whether Muse's drop shows.
    #[serde(default)]
    pub muse: bool,
    /// Your dot's look, or `None` when its drop doesn't show.
    #[serde(default)]
    pub dot: Option<DotLook>,
}

/// A change from the corral's right-click menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LinkChange {
    /// Show an app's drop. For your dot, `look` picks its look; `None` keeps the current one (or
    /// the ring).
    Show {
        /// The app.
        app: LinkedApp,
        /// Your dot's look.
        look: Option<DotLook>,
    },
    /// Take an app's drop away.
    Hide(LinkedApp),
}

impl LinkedSettings {
    /// True when nothing is linked.
    pub fn is_empty(&self) -> bool {
        !self.muse && self.dot.is_none()
    }

    /// The apps whose drops show, in order.
    pub fn shown(&self) -> Vec<LinkedApp> {
        LinkedApp::ALL
            .into_iter()
            .filter(|app| match app {
                LinkedApp::Muse => self.muse,
                LinkedApp::Dot => self.dot.is_some(),
            })
            .collect()
    }

    /// The picture for a shown app's drop.
    pub fn logo(&self, app: LinkedApp) -> Option<LogoId> {
        match app {
            LinkedApp::Muse => self.muse.then_some(LogoId::Muse),
            LinkedApp::Dot => self.dot.map(LogoId::Dot),
        }
    }

    /// Applies a change; true if anything changed.
    pub fn apply(&mut self, change: LinkChange) -> bool {
        let before = *self;
        match change {
            LinkChange::Show {
                app: LinkedApp::Muse,
                look: _,
            } => self.muse = true,
            LinkChange::Show {
                app: LinkedApp::Dot,
                look,
            } => self.dot = look.or(self.dot).or(Some(DotLook::Ring)),
            LinkChange::Hide(LinkedApp::Muse) => self.muse = false,
            LinkChange::Hide(LinkedApp::Dot) => self.dot = None,
        }
        *self != before
    }
}

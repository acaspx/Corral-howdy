//! What the corral shows, kept from the event log (SK-6, TP-USAGE-06..12, TP-CHK-22): the drops
//! and their order, the run cards, the logos and linked apps, and the checkpoint notifications
//! that are out. Each change says whether the corral changed, and [`CorralState::snapshot`] is
//! the roster Swift gets.
//!
//! The native shell (`surface.rs`) holds one behind a lock and sends the roster when it changed;
//! the end-to-end journeys (`tests/journeys.rs`, Spike S2) drive the same state without the
//! native layer, so a journey's transcript is what the app sends.

use crate::bridge::{PromptBarSnapshot, RosterSnapshot};
use crate::corral_order::CorralOrder;
use crate::prompt_bar_feed;
use corral_adapters::linked::LinkedSettings;
use corral_core::checkpoint_note::{NoteDesk, Notice};
use corral_core::corral_view::{AgentCard, CorralTracker, DropState};
use corral_core::event::{Event, EventBody};
use corral_core::ids::{AgentId, CheckpointId};
use corral_core::prompt_bar::BarAgent;
use corral_core::run_card::{EditError, RunCards};
use corral_core::trace_tree::trace_tree;
use corral_core::usage_rank::Deploy;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};

/// Makes each checkpoint notification's single-use nonce.
pub type Nonces = Box<dyn FnMut() -> String + Send>;

/// The corral's state.
pub struct CorralState {
    pub(crate) corral: CorralTracker,
    /// The corral's usage order and shown set (TP-USAGE-06..09).
    pub(crate) order: CorralOrder,
    /// Every agent, before the order picks and sorts the shown ones.
    pub(crate) all_cards: Vec<AgentCard>,
    pub(crate) cards: RunCards,
    /// Logo paths by agent.
    pub(crate) logos: HashMap<AgentId, String>,
    /// Which linked apps show, for the right-click menu.
    pub(crate) linked: LinkedSettings,
    /// The checkpoint notifications that are out (TP-CHK-22).
    pub(crate) notes: NoteDesk,
    nonces: Nonces,
}

impl CorralState {
    /// The corral for `all_cards` and the `linked` apps, placed by `order` and saved. Nonces are
    /// random.
    pub fn open(
        mut order: CorralOrder,
        all_cards: Vec<AgentCard>,
        linked: LinkedSettings,
        now_ms: u64,
    ) -> Self {
        let placed = order.place(all_cards.clone(), now_ms);
        if placed.choose_shown {
            eprintln!(
                "corral: the corral holds up to 8 agents; a new one is hidden until you choose"
            );
        }
        save_order(&mut order);
        Self {
            corral: CorralTracker::new(placed.cards),
            order,
            all_cards,
            cards: RunCards::default(),
            logos: HashMap::new(),
            linked,
            notes: NoteDesk::default(),
            nonces: Box::new(crate::agent_tools::random_id),
        }
    }

    /// This state, with notification nonces from `nonces` (a journey's fixed ones).
    pub fn with_nonces(mut self, nonces: Nonces) -> Self {
        self.nonces = nonces;
        self
    }

    /// Replaces the corral's agents; the usage order picks and sorts the shown ones
    /// (TP-USAGE-07, 08). True when the corral changed.
    pub fn set_agents(
        &mut self,
        cards: Vec<AgentCard>,
        linked: LinkedSettings,
        now_ms: u64,
    ) -> bool {
        self.all_cards = cards.clone();
        let placed = self.order.place(cards, now_ms);
        if placed.choose_shown {
            eprintln!(
                "corral: the corral holds up to 8 agents; a new one is hidden until you choose"
            );
        }
        save_order(&mut self.order);
        let drops_changed = self.corral.set_agents(placed.cards);
        let links_changed = self.linked != linked;
        self.linked = linked;
        drops_changed || links_changed
    }

    /// At launch, once the store is open: the last 90 days of deploys, and the order they give
    /// (TP-USAGE-06). True when the corral changed.
    pub fn seed_usage(&mut self, deploys: Vec<Deploy>, now_ms: u64) -> bool {
        if !self.order.seed(deploys, now_ms) {
            return false;
        }
        save_order(&mut self.order);
        let cards = self.all_cards.clone();
        let placed = self.order.place(cards, now_ms);
        self.corral.set_agents(placed.cards)
    }

    /// Swift's report that the corral turned idle or busy (TP-USAGE-10). Idle with an order
    /// waiting and nothing waiting on the user, the new order takes. True when the corral changed.
    pub fn corral_idle(&mut self, idle: bool, now_ms: u64) -> bool {
        self.order.set_corral_idle(idle);
        self.settle(now_ms)
    }

    /// Shows `agent`'s logo from `path`. True when it's new.
    pub fn set_logo(&mut self, agent: AgentId, path: String) -> bool {
        if self.logos.get(&agent) == Some(&path) {
            return false;
        }
        self.logos.insert(agent, path);
        true
    }

    /// Applies one logged event: whether a drop, a card or the order changed (send the roster
    /// then), and the checkpoint notifications to post.
    pub fn observe(&mut self, event: &Event, now_ms: u64) -> (bool, Vec<Notice>) {
        // Deploys count toward the usage order at once; a new order waits for the corral to be
        // idle (TP-USAGE-07).
        if let EventBody::RunQueued(queued) = &event.body {
            self.order.deployed(queued.agent_id.clone(), event.ts);
        }
        if let EventBody::RunCompleted(_) | EventBody::RunFailed(_) | EventBody::RunCancelled(_) =
            &event.body
        {
            self.order.run_ended(event.ts);
        }
        // The supervisor already validated the event; a refusal here means the views diverged.
        let drop_changed = self.corral.observe(event).unwrap_or_else(|e| {
            eprintln!("corral: the corral view refused {:?}: {e}", event.kind());
            false
        });
        let card_changed = self.cards.observe(event).unwrap_or_else(|e| {
            eprintln!("corral: the run cards refused {:?}: {e}", event.kind());
            false
        });
        // A run ending or a checkpoint answered can leave the corral idle with an order waiting.
        let order_changed = match &event.body {
            EventBody::RunCompleted(_)
            | EventBody::RunFailed(_)
            | EventBody::RunCancelled(_)
            | EventBody::CheckpointResolved(_) => self.settle(now_ms),
            EventBody::RunQueued(_)
            | EventBody::RunStarted(_)
            | EventBody::PlanUpdated(_)
            | EventBody::ModelRequest(_)
            | EventBody::ModelDelta(_)
            | EventBody::ModelResponse(_)
            | EventBody::ToolRequested(_)
            | EventBody::PolicyDecided(_)
            | EventBody::CheckpointOpened(_)
            | EventBody::ToolStarted(_)
            | EventBody::ToolResult(_)
            | EventBody::UsageReported(_)
            | EventBody::BudgetWarned(_)
            | EventBody::RunPaused(_)
            | EventBody::RunResumed(_)
            | EventBody::SessionClosed(_) => false,
        };
        let notices = if card_changed {
            self.notices()
        } else {
            Vec::new()
        };
        (drop_changed || card_changed || order_changed, notices)
    }

    /// The roster Swift gets now.
    pub fn snapshot(&self, now_ms: u64) -> RosterSnapshot {
        let agents: Vec<AgentId> = self.all_cards.iter().map(|card| card.id.clone()).collect();
        RosterSnapshot::with_cards(&self.corral.view(), &self.cards, now_ms)
            .with_plans(&self.cards)
            .with_queue(&self.cards)
            .with_trace(&trace_tree(&self.cards, &agents))
            .with_column_names(|id| {
                self.all_cards
                    .iter()
                    .find(|card| &card.id == id)
                    .map(|card| card.name.clone())
            })
            .with_logos(|id| self.logos.get(id).cloned())
            .with_linked(&self.linked)
    }

    /// Whether `checkpoint` waits on `agent`'s card, as the user stepped to it: the only one an
    /// answer from that card may be for (TP-HOVER-06, TP-CHK-19).
    pub fn waits_on(&self, agent: &AgentId, checkpoint: &CheckpointId) -> bool {
        self.cards.waits_on(agent, checkpoint)
    }

    /// The arguments an edit sheet's `changes` make for `checkpoint` on `agent`'s card, checked
    /// by the core (TP-CHK-15).
    pub fn edited_args(
        &self,
        agent: &AgentId,
        checkpoint: &CheckpointId,
        changes: &BTreeMap<String, String>,
    ) -> Result<Value, EditError> {
        self.cards.edited_args(agent, checkpoint, changes)
    }

    /// Every agent the prompt bar knows, in the corral's order, with what each card says now
    /// (TP-BAR-08).
    pub fn bar_agents(&self, now_ms: u64) -> Vec<BarAgent> {
        let settings = self.order.settings(&self.all_cards);
        prompt_bar_feed::bar_agents(&settings, |id| {
            self.cards.card(id, now_ms).map(|c| c.status)
        })
    }

    /// The prompt bar's answer, with each suggestion's logo.
    pub fn with_bar_logos(&self, answer: PromptBarSnapshot) -> PromptBarSnapshot {
        answer.with_logos(|id| self.logos.get(id).cloned())
    }

    /// Publishes a waiting usage order if the corral is idle and no drop waits on the user
    /// (TP-USAGE-12), and re-places the drops. True when the corral changed.
    pub(crate) fn settle(&mut self, now_ms: u64) -> bool {
        let waiting = self
            .corral
            .view()
            .drops
            .iter()
            .any(|drop| drop.state == DropState::NeedsYou);
        if !self.order.settle(waiting) {
            return false;
        }
        save_order(&mut self.order);
        let cards = self.all_cards.clone();
        let placed = self.order.place(cards, now_ms);
        self.corral.set_agents(placed.cards)
    }

    /// The notifications to post for checkpoints that started waiting, each with a nonce, and
    /// the names the drops show.
    fn notices(&mut self) -> Vec<Notice> {
        let names: HashMap<AgentId, String> = self
            .corral
            .view()
            .drops
            .into_iter()
            .map(|drop| (drop.agent.id, drop.agent.name))
            .collect();
        let name = |agent: &AgentId| {
            names
                .get(agent)
                .cloned()
                .unwrap_or_else(|| agent.to_string())
        };
        self.notes.observe(&self.cards, &name, &mut self.nonces)
    }
}

/// Saves the corral's order. A failure is logged; the corral keeps working unsaved.
pub(crate) fn save_order(order: &mut CorralOrder) {
    if let Err(e) = order.save() {
        eprintln!("corral: {e}");
    }
}

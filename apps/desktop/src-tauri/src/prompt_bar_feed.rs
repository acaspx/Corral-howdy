//! The prompt bar's glue (TP-BAR-08): the agents the bar knows, from the corral's settings and its
//! run cards, and the answer Rust sends Swift for an edit or a send. The rules are the core's
//! (`corral_core::prompt_bar`, AGENTS.md rule 3); the native shell only passes these along.

use crate::bridge::{BarEdit, BarOutcome, PromptBarSnapshot};
use crate::corral_order::CorralSettingsView;
use corral_core::controls::ControlError;
use corral_core::ids::{AgentId, RunId};
use corral_core::mention::Addressee;
use corral_core::prompt_bar::{self, BarAgent, BarStatus, How};
use corral_core::run_card::CardStatus;

/// Whether an agent with this card can take a task: ready unless it's working or waiting on you.
pub fn bar_status(card: Option<CardStatus>) -> BarStatus {
    match card {
        None | Some(CardStatus::Done | CardStatus::Error | CardStatus::Stopped) => BarStatus::Ready,
        Some(CardStatus::Queued | CardStatus::Working) => BarStatus::Working,
        Some(CardStatus::NeedsYou) => BarStatus::NeedsYou,
    }
}

/// Every agent, in the corral's order (shown ones first), with what each card says now.
pub fn bar_agents(
    settings: &CorralSettingsView,
    card: impl Fn(&AgentId) -> Option<CardStatus>,
) -> Vec<BarAgent> {
    settings
        .agents
        .iter()
        .map(|choice| {
            let id = AgentId::from(choice.id.as_str());
            let status = bar_status(card(&id));
            BarAgent {
                agent: Addressee {
                    id,
                    name: choice.name.clone(),
                    shown: choice.shown,
                },
                status,
            }
        })
        .collect()
}

/// The answer to an edit: what the bar shows for its text and cursor.
pub fn edited(edit: &BarEdit, agents: &[BarAgent]) -> PromptBarSnapshot {
    let before_cursor = edit.text.get(..edit.cursor).unwrap_or(&edit.text);
    PromptBarSnapshot::new(
        edit.seq,
        prompt_bar::view(&edit.text, before_cursor, &edit.selection, agents),
        None,
    )
}

/// Sends the bar's prompt through `deploy` (the corral's controls) and answers with how it went
/// and what the bar shows next: an empty field when someone got it, the same text when no one
/// did.
pub fn sent(
    edit: &BarEdit,
    how: How,
    agents: &[BarAgent],
    deploy: impl FnMut(&AgentId, &str) -> Result<RunId, ControlError>,
) -> PromptBarSnapshot {
    let (outcome, text) = match prompt_bar::send(&edit.text, &edit.selection, agents, how, deploy) {
        Ok(sent) => (sent.note.map_or(BarOutcome::Sent, BarOutcome::Note), ""),
        Err(why) => (BarOutcome::Refused(why), edit.text.as_str()),
    };
    PromptBarSnapshot::new(
        edit.seq,
        prompt_bar::view(text, text, &edit.selection, agents),
        Some(outcome),
    )
}

/// What the bar says when tasks are off (the event log or the agents didn't start).
pub const TASKS_OFF: &str =
    "Corral can't run tasks right now. Restart it, and if this keeps happening, check its log.";

/// The answer to a send when tasks are off: nothing is sent and the text stays.
pub fn tasks_off(edit: &BarEdit, agents: &[BarAgent]) -> PromptBarSnapshot {
    PromptBarSnapshot::new(
        edit.seq,
        prompt_bar::view(&edit.text, &edit.text, &edit.selection, agents),
        Some(BarOutcome::Refused(TASKS_OFF.to_owned())),
    )
}

// TP-BAR-08: the agents the bar knows, and the answers to an edit and a send.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::corral_order::CorralAgentChoice;

    fn settings() -> CorralSettingsView {
        let choice = |id: &str, name: &str, shown: bool| CorralAgentChoice {
            id: id.to_owned(),
            name: name.to_owned(),
            shown,
        };
        CorralSettingsView {
            agents: vec![
                choice("grok", "Grok", true),
                choice("claude", "Claude", true),
                choice("muse", "Muse Spark", false),
            ],
            keep_fixed: false,
            needs_choice: false,
            max_shown: 8,
        }
    }

    fn edit(seq: u32, text: &str, selection: &[&str]) -> BarEdit {
        BarEdit {
            seq,
            text: text.to_owned(),
            cursor: text.len(),
            selection: selection.iter().map(|id| AgentId::from(*id)).collect(),
        }
    }

    #[test]
    fn tp_bar_08_the_bar_knows_every_agent_in_the_corrals_order_with_its_status() {
        let agents = bar_agents(&settings(), |id| match id.as_str() {
            "grok" => Some(CardStatus::Working),
            "muse" => Some(CardStatus::NeedsYou),
            _ => None,
        });
        let seen: Vec<(&str, &str, bool, BarStatus)> = agents
            .iter()
            .map(|a| {
                (
                    a.agent.id.as_str(),
                    a.agent.name.as_str(),
                    a.agent.shown,
                    a.status,
                )
            })
            .collect();
        assert_eq!(
            seen,
            [
                ("grok", "Grok", true, BarStatus::Working),
                ("claude", "Claude", true, BarStatus::Ready),
                ("muse", "Muse Spark", false, BarStatus::NeedsYou)
            ]
        );
    }

    #[test]
    fn tp_bar_08_a_card_status_is_ready_unless_the_agent_is_busy() {
        for (card, bar) in [
            (None, BarStatus::Ready),
            (Some(CardStatus::Queued), BarStatus::Working),
            (Some(CardStatus::Working), BarStatus::Working),
            (Some(CardStatus::NeedsYou), BarStatus::NeedsYou),
            (Some(CardStatus::Done), BarStatus::Ready),
            (Some(CardStatus::Error), BarStatus::Ready),
            (Some(CardStatus::Stopped), BarStatus::Ready),
        ] {
            assert_eq!(bar_status(card), bar, "{card:?}");
        }
    }

    #[test]
    fn tp_bar_08_an_edit_is_answered_with_the_view_and_its_number() {
        let agents = bar_agents(&settings(), |_| None);
        let mut typing = edit(7, "@cl plan", &[]);
        typing.cursor = 3;
        let answer = edited(&typing, &agents);
        assert_eq!(answer.seq(), 7);
        assert_eq!(answer.view().suggestions.len(), 1, "from the cursor");
        assert_eq!(answer.outcome(), None);
    }

    #[test]
    fn tp_bar_08_a_send_is_answered_with_its_outcome_and_the_text_it_leaves() {
        let agents = bar_agents(&settings(), |_| None);
        let deploy_busy = |busy: &'static [&'static str]| {
            move |agent: &AgentId, _: &str| {
                if busy.contains(&agent.as_str()) {
                    Err(ControlError::Busy(agent.clone()))
                } else {
                    Ok(RunId::from("run-1"))
                }
            }
        };

        let done = sent(
            &edit(8, "@grok go", &[]),
            How::Return,
            &agents,
            deploy_busy(&[]),
        );
        assert_eq!((done.seq(), done.outcome()), (8, Some(&BarOutcome::Sent)));
        assert!(done.view().chips.is_empty(), "the view of the cleared text");

        let noted = sent(
            &edit(9, "@grok @claude go", &[]),
            How::Return,
            &agents,
            deploy_busy(&["grok"]),
        );
        assert_eq!(
            noted.outcome(),
            Some(&BarOutcome::Note(
                "Sent to Claude. Grok is busy.".to_owned()
            ))
        );
        assert!(
            noted.view().chips.is_empty(),
            "the text is cleared: Claude has the task"
        );

        let refused = sent(
            &edit(10, "@grok go", &["claude"]),
            How::Return,
            &agents,
            deploy_busy(&["grok"]),
        );
        assert_eq!(
            refused.outcome(),
            Some(&BarOutcome::Refused("Grok is busy.".to_owned()))
        );
        assert_eq!(refused.view().chips, ["Grok"], "the view of the text, kept");

        let kept = sent(
            &edit(11, "@grok go", &["claude"]),
            How::Return,
            &agents,
            deploy_busy(&[]),
        );
        assert_eq!(
            kept.view().chips,
            ["Claude"],
            "after sending, the selection still shows"
        );
    }

    #[test]
    fn tp_bar_08_with_tasks_off_a_send_is_refused_and_the_text_stays() {
        let agents = bar_agents(&settings(), |_| None);
        let answer = tasks_off(&edit(12, "@grok go", &[]), &agents);
        assert_eq!(answer.seq(), 12);
        assert_eq!(
            answer.outcome(),
            Some(&BarOutcome::Refused(TASKS_OFF.to_owned()))
        );
        assert_eq!(answer.view().chips, ["Grok"]);
    }
}

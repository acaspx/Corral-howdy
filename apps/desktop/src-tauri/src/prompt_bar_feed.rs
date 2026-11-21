//! The prompt bar's glue (TP-BAR-08): the agents the bar knows, from the corral's settings and its
//! run cards, and the answer Rust sends Swift for an edit or a send. The rules are the core's
//! (`corral_core::prompt_bar`, AGENTS.md rule 3); the native shell only passes these along.

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
}

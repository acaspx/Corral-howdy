// CorralKeyboardTests.swift: the keyboard path and VoiceOver (docs/07 §10, M4 item 8).
// TP-A11Y-01..03 and TP-HOVER-07: summon → ↓ into the corral → ←→ between drops → Space opens the
// card pinned, focus on its first button → Esc back. Pure logic, plus the card panel's role.
import Testing
import AppKit
@testable import CorralNative

private let ranked = ["grok", "claude", "scout"]

// MARK: TP-A11Y-01 arrows move between drops in rank order; Space opens; ↑ and Esc go back

@Test("TP-A11Y-01 → and ↓ go to the next drop by rank, ← to the one before, wrapping")
func arrowsFollowRank() {
    #expect(CorralKeyboard.move(.right, from: "grok", ranked: ranked) == .focus("claude"))
    #expect(CorralKeyboard.move(.down, from: "claude", ranked: ranked) == .focus("scout"))
    #expect(CorralKeyboard.move(.right, from: "scout", ranked: ranked) == .focus("grok"), "wraps")
    #expect(CorralKeyboard.move(.left, from: "grok", ranked: ranked) == .focus("scout"), "wraps back")
    #expect(CorralKeyboard.move(.left, from: "scout", ranked: ranked) == .focus("claude"))
}

@Test("TP-A11Y-01 with no drop focused, or one that's gone, an arrow starts at the most used")
func arrowsStartAtTheTop() {
    #expect(CorralKeyboard.move(.right, from: nil, ranked: ranked) == .focus("grok"))
    #expect(CorralKeyboard.move(.left, from: "gone", ranked: ranked) == .focus("grok"))
    #expect(CorralKeyboard.entry(ranked: ranked) == "grok")
    #expect(CorralKeyboard.entry(ranked: []) == nil)
    #expect(CorralKeyboard.move(.right, from: nil, ranked: []) == .none, "an empty corral")
}

@Test("TP-A11Y-01 Space or Return opens the focused drop's card; ↑ and Esc go back to the bar")
func openAndLeave() {
    #expect(CorralKeyboard.move(.space, from: "claude", ranked: ranked) == .open("claude"))
    #expect(CorralKeyboard.move(.enter, from: "claude", ranked: ranked) == .open("claude"))
    #expect(CorralKeyboard.move(.space, from: nil, ranked: ranked) == .none, "nothing focused")
    #expect(CorralKeyboard.move(.space, from: "gone", ranked: ranked) == .none)
    #expect(CorralKeyboard.move(.up, from: "claude", ranked: ranked) == .toBar)
    #expect(CorralKeyboard.move(.escape, from: "claude", ranked: ranked) == .toBar)
    #expect(CorralKeyboard.move(.escape, from: nil, ranked: []) == .toBar)
}

@Test("TP-A11Y-01 a usage reorder keeps focus on the same drop; the arrows follow the new order")
func reorderKeepsFocus() {
    let reordered = ["scout", "grok", "claude"]
    #expect(CorralKeyboard.kept("claude", ranked: reordered) == "claude")
    #expect(CorralKeyboard.kept("gone", ranked: reordered) == nil)
    #expect(CorralKeyboard.kept(nil, ranked: reordered) == nil)
    #expect(CorralKeyboard.move(.right, from: "claude", ranked: reordered) == .focus("scout"))
}

// MARK: TP-A11Y-02 ↓ in the prompt bar, with no suggestion list, enters the corral

@Test("TP-A11Y-02 ↓ with no list moves focus to the corral; with a list it moves the highlight")
func downEntersTheCorral() {
    let rows = [CorralPromptBarView.Suggestion(id: "claude", name: "Claude", shown: true,
                                               status: .ready, completion: "Claude", logoPath: nil)]
    var bar = CorralPromptBarState()
    _ = bar.open(naming: nil)
    #expect(bar.key(.down, suggestions: []) == .toCorral)
    _ = bar.edit(text: "@c", cursor: 2)
    #expect(bar.key(.down, suggestions: rows) == .none, "the list keeps ↓")
    #expect(bar.isOpen, "the bar stays open behind the corral")
}

// MARK: TP-HOVER-07 the pinned card's focus goes to its first button

private func card(risk: CorralRunCard.Checkpoint.Risk, detail: String = "a@example.test") -> CorralRunCard {
    CorralRunCard(status: .needsYou, prompt: "Tidy my inbox", elapsedMs: 0, spend: "$0.00",
                  actions: [.approve, .deny, .stop],
                  checkpoint: .init(id: "cp-1", actionClass: "comms.send", risk: risk,
                                    title: "Email", detail: detail))
}

@Test("TP-HOVER-07 with a checkpoint waiting, focus goes to Approve when it can be pressed")
func focusOnApprove() {
    let normal = CorralCardModel(agentName: "Scout", card: card(risk: .normal))
    #expect(normal.firstFocus(pinned: true, expanded: false) == .button(.approve(checkpoint: "cp-1", touchID: false)))
    let critical = CorralCardModel(agentName: "Scout", card: card(risk: .critical))
    #expect(critical.firstFocus(pinned: true, expanded: false) == .button(.approve(checkpoint: "cp-1", touchID: true)))
}

@Test("TP-HOVER-07 when Approve can't be pressed yet, focus goes to the next button, never to Approve")
func focusSkipsADisabledApprove() {
    let long = String(repeating: "x", count: CorralCardModel.Approval.collapsedCharacters + 1)
    let high = CorralCardModel(agentName: "Scout", card: card(risk: .high, detail: long))
    #expect(high.firstFocus(pinned: true, expanded: false) == .button(.deny(checkpoint: "cp-1")),
            "the detail isn't expanded yet")
    #expect(high.firstFocus(pinned: true, expanded: true) == .button(.approve(checkpoint: "cp-1", touchID: false)))
}

@Test("TP-HOVER-07 with nothing waiting, focus goes to the task field; a hover card takes none")
func focusOnTheField() {
    let idle = CorralCardModel(agentName: "Scout", card: nil)
    #expect(idle.firstFocus(pinned: true, expanded: false) == .task)
    #expect(idle.firstFocus(pinned: false, expanded: false) == nil, "hovering never takes focus")
    #expect(CorralCardModel(agentName: "Scout", card: card(risk: .normal)).firstFocus(pinned: false, expanded: false) == nil)
}

// MARK: TP-A11Y-03 VoiceOver: drops in rank order; the card is a dialog named for its agent

@Test("TP-A11Y-03 VoiceOver reads the drops in rank order, the most used first")
func voiceOverOrder() {
    #expect(CorralKeyboard.voiceOverPriority(rank: 1) > CorralKeyboard.voiceOverPriority(rank: 2))
    #expect(CorralKeyboard.voiceOverPriority(rank: 2) > CorralKeyboard.voiceOverPriority(rank: 8))
}

@Test("TP-A11Y-03 the card is announced as a dialog named for its agent")
@MainActor
func cardIsADialog() {
    #expect(CorralCardPanel().accessibilitySubrole() == .dialog)
    #expect(CorralCardModel(agentName: "Scout", card: nil).dialogLabel == "Scout's card")
}

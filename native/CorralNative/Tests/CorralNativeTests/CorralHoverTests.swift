// CorralHoverTests.swift: TP-HOVER-01, 02, 05 (docs/04-test-plan.md), Spike S6.
// The hover card's timing, the buttons and field for each state, and the click-through guard.
// Pure logic: no window, no clock (time is passed in).
import Testing
import Foundation
@testable import CorralNative

// MARK: TP-HOVER-01 hover opens after 120 ms, closes 260 ms after leaving, a click pins

@Test("TP-HOVER-01 hovering a drop opens its card after 120 ms, not before")
func hoverOpensAfterDelay() {
    var s = CorralHoverState()
    s.pointer(overDrop: "a", at: 10.0)
    s.tick(at: 10.1)
    #expect(s.shown == nil)
    #expect(abs((s.deadline ?? 0) - 10.12) < 1e-9)
    s.tick(at: 10.121)
    #expect(s.shown == "a")
    #expect(!s.pinned)
}

@Test("TP-HOVER-01 passing over a drop quickly opens nothing")
func quickPassOpensNothing() {
    var s = CorralHoverState()
    s.pointer(overDrop: "a", at: 10.0)
    s.pointer(overDrop: nil, at: 10.05)
    s.tick(at: 11.0)
    #expect(s.shown == nil)
}

@Test("TP-HOVER-01 the card closes 260 ms after the pointer leaves, and stays if it comes back")
func leaveClosesAfterDelay() {
    var s = CorralHoverState()
    s.pointer(overDrop: "a", at: 0)
    s.tick(at: 0.2)
    s.pointer(overDrop: nil, at: 1.0)
    s.tick(at: 1.25)
    #expect(s.shown == "a", "still open just before 260 ms")
    s.pointer(overDrop: "a", at: 1.255)
    s.tick(at: 2.0)
    #expect(s.shown == "a", "came back in time")
    s.pointer(overDrop: nil, at: 3.0)
    s.tick(at: 3.261)
    #expect(s.shown == nil)
}

@Test("TP-HOVER-01 moving from the drop onto the card keeps it open")
func cardKeepsItOpen() {
    var s = CorralHoverState()
    s.pointer(overDrop: "a", at: 0)
    s.tick(at: 0.2)
    s.pointer(overDrop: nil, at: 1.0)
    s.pointer(overCard: true, at: 1.1)
    s.tick(at: 5.0)
    #expect(s.shown == "a")
    s.pointer(overCard: false, at: 6.0)
    s.tick(at: 6.261)
    #expect(s.shown == nil)
}

@Test("TP-HOVER-01 hovering another drop swaps an unpinned card")
func hoverSwaps() {
    var s = CorralHoverState()
    s.pointer(overDrop: "a", at: 0)
    s.tick(at: 0.2)
    s.pointer(overDrop: "b", at: 1.0)
    s.tick(at: 1.121)
    #expect(s.shown == "b")
}

@Test("TP-HOVER-01 a click pins the card: leaving and hovering other drops don't move it")
func clickPins() {
    var s = CorralHoverState()
    s.pointer(overDrop: "a", at: 0)
    s.click(drop: "a")
    #expect(s.shown == "a")
    #expect(s.pinned)
    s.pointer(overDrop: nil, at: 1.0)
    s.pointer(overDrop: "b", at: 1.1)
    s.tick(at: 9.0)
    #expect(s.shown == "a")
    #expect(s.deadline == nil, "nothing pending while pinned")
}

@Test("TP-HOVER-01 clicking another drop pins its card instead")
func clickAnotherDrop() {
    var s = CorralHoverState()
    s.click(drop: "a")
    s.click(drop: "b")
    #expect(s.shown == "b")
    #expect(s.pinned)
}

@Test("TP-HOVER-01 ×, Esc and a click elsewhere close a pinned card")
func dismissCloses() {
    var s = CorralHoverState()
    s.click(drop: "a")
    s.dismiss()
    #expect(s.shown == nil)
    #expect(!s.pinned)
    // The pointer is still over the drop: hovering resumes from the next movement.
    s.pointer(overDrop: "b", at: 1.0)
    s.tick(at: 1.121)
    #expect(s.shown == "b")
    #expect(!s.pinned)
}

// MARK: TP-HOVER-02 buttons and the task field match the state table (07 §8.2)

private func card(_ status: CorralRunCard.Status, actions: [CorralRunCard.Action],
                  answer: String? = nil, error: String? = nil) -> CorralRunCard {
    CorralRunCard(status: status, prompt: "What is the capital of France?", elapsedMs: 76_000,
                  spend: "$0.0004", answer: answer, error: error, actions: actions)
}

@Test("TP-HOVER-02 an agent in the corral offers a task field and no buttons")
func readyCard() {
    let m = CorralCardModel(agentName: "Scout", card: nil)
    #expect(m.label == "SCOUT")
    #expect(m.headline == "Ready for a task")
    #expect(m.buttons.isEmpty)
    #expect(m.fieldPlaceholder == "Give Scout a task")
}

@Test("TP-HOVER-02 a working card offers Stop and no field")
func workingCard() {
    let m = CorralCardModel(agentName: "Scout", card: card(.working, actions: [.stop]))
    #expect(m.headline == "What is the capital of France?")
    #expect(m.status == "Working · 1m 16s · $0.0004")
    #expect(m.buttons == [.stop])
    #expect(m.fieldPlaceholder == nil)
}

@Test("TP-HOVER-02 a done card shows the answer, Send back, and a follow-up field")
func doneCard() {
    let m = CorralCardModel(agentName: "Scout",
                            card: card(.done, actions: [.sendBack, .newTask], answer: "Paris."))
    #expect(m.status == "Done in 1m 16s · $0.0004")
    #expect(m.answer == "Paris.")
    #expect(m.buttons == [.sendBack])
    #expect(m.fieldPlaceholder == "Ask a follow-up")
}

@Test("TP-HOVER-02 an error card shows why, with Retry first")
func errorCard() {
    let m = CorralCardModel(agentName: "Scout",
                            card: card(.error, actions: [.retry, .sendBack], error: "Add your xAI key."))
    #expect(m.error == "Add your xAI key.")
    #expect(m.buttons == [.retry, .sendBack])
    #expect(m.buttons.first?.isPrimary == true)
    #expect(m.fieldPlaceholder == nil)
}

@Test("TP-HOVER-02 a stopped card offers Send back and a new task")
func stoppedCard() {
    let m = CorralCardModel(agentName: "Scout", card: card(.stopped, actions: [.sendBack, .newTask]))
    #expect(m.status == "Stopped after 1m 16s")
    #expect(m.buttons == [.sendBack])
    #expect(m.fieldPlaceholder == "Give Scout a new task")
}

@Test("TP-HOVER-02 buttons and the field send the matching intents")
func cardIntents() {
    #expect(CorralCardModel.intent(for: .stop, agentID: "a") == .stop(id: "a"))
    #expect(CorralCardModel.intent(for: .sendBack, agentID: "a") == .sendBack(id: "a"))
    #expect(CorralCardModel.intent(for: .retry, agentID: "a") == .retry(id: "a"))
    #expect(CorralCardModel.task("  Plan my week  ", agentID: "a") == .deployTask(id: "a", prompt: "Plan my week"))
    #expect(CorralCardModel.task(" \n ", agentID: "a") == nil, "an empty task sends nothing")
}

@Test("TP-HOVER-02 elapsed time reads plainly")
func elapsedText() {
    #expect(CorralCardModel.duration(ms: 0) == "0s")
    #expect(CorralCardModel.duration(ms: 5_400) == "5s")
    #expect(CorralCardModel.duration(ms: 76_000) == "1m 16s")
    #expect(CorralCardModel.duration(ms: 3_720_000) == "1h 2m")
}

// MARK: TP-HOVER-05 buttons ignore input for 300 ms after the card appears

@Test("TP-HOVER-05 a card that just appeared can't be clicked through")
func inputGuard() {
    let guardrail = CorralInputGuard(shownAt: 100.0)
    #expect(!guardrail.accepts(at: 100.0))
    #expect(!guardrail.accepts(at: 100.29))
    #expect(guardrail.accepts(at: 100.31))
}

// MARK: TP-SKEL-04 the working ring and status badges (07 §8.1)

@Test("TP-SKEL-04 a working drop turns a ring; done, error and needs-you get badges; never color alone")
func statusMarks() {
    #expect(CorralStatusMark.of(.available) == .none)
    #expect(CorralStatusMark.of(.working) == .turningArc)
    #expect(CorralStatusMark.of(.needsYou) == .pulse(badge: "exclamationmark"))
    #expect(CorralStatusMark.of(.done) == .badge("checkmark"))
    #expect(CorralStatusMark.of(.error) == .badge("exclamationmark.triangle.fill"))
}

// CorralAllowForRunTests.swift: "Allow for this run" in the approval box (M2 item 4, TP-CHK-09).
// Rust re-checks every answer: the policy pins a high-risk grant to the destination, and a run
// that read untrusted content asks again on the classes that send data out. The card only offers
// it where it means something, under the same rules as Approve.
import Testing
@testable import CorralNative

private func card(risk: CorralRunCard.Checkpoint.Risk, actionClass: String = "comms.send") -> CorralCardModel {
    CorralCardModel(agentName: "Scout", card: CorralRunCard(
        status: .needsYou, prompt: "hi", elapsedMs: 1_000, spend: "$0.00",
        actions: [.approve, .deny],
        checkpoint: CorralRunCard.Checkpoint(
            id: "cp-1", actionClass: actionClass, risk: risk,
            title: "Sending a message or email", detail: "email.send to a@example.test")))
}

@Test("TP-CHK-09 normal and high-risk checkpoints offer Allow for this run; high risk says it's this destination only")
func forRunIsOffered() throws {
    let normal = try #require(card(risk: .normal).approval)
    #expect(normal.forRunTitle == "Allow for this run")
    let high = try #require(card(risk: .high).approval)
    #expect(high.forRunTitle == "Allow for this run (this destination only)")
    #expect(card(risk: .high).buttons == [.approve(checkpoint: "cp-1", touchID: false), .deny(checkpoint: "cp-1")],
            "it sits in the approval box, not in the button row")
}

@Test("TP-CHK-09 critical checkpoints and the budget limit don't offer it")
func forRunIsNotOffered() throws {
    #expect(try #require(card(risk: .critical).approval).forRunTitle == nil, "Touch ID every time")
    #expect(try #require(card(risk: .normal, actionClass: "budget.threshold").approval).forRunTitle == nil,
            "a budget limit is answered once")
    let working = CorralCardModel(agentName: "Scout", card: CorralRunCard(
        status: .working, prompt: "hi", elapsedMs: 0, spend: "$0.00", actions: [.stop]))
    #expect(working.approval == nil)
}

@Test("TP-CHK-09 Allow for this run sends the checkpoint's ID with the run scope")
func forRunIntent() {
    #expect(CorralCardModel.Button.allowForRun(checkpoint: "cp-1").title == "Allow for this run")
    #expect(CorralCardModel.intent(for: .allowForRun(checkpoint: "cp-1"), agentID: "a")
            == .approveCheckpoint(id: "a", checkpoint: "cp-1", forRun: true))
}

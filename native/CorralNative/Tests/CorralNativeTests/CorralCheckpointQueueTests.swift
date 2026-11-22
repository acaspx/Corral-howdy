// CorralCheckpointQueueTests.swift: several checkpoints waiting on one card (docs/07 §8.2,
// TP-CHK-20, TP-CHK-21). The approval box steps through them, "1 of 3" with arrows; the buttons
// and the edit sheet are for the one shown. The oldest shows first.
import Testing
import Foundation
@testable import CorralNative

private func fixture(_ name: String) throws -> Data {
    let root = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    return try Data(contentsOf: root.appendingPathComponent("Fixtures/bridge/\(name)"))
}

private func checkpoint(_ id: String, _ risk: CorralRunCard.Checkpoint.Risk) -> CorralRunCard.Checkpoint {
    .init(id: id, actionClass: "comms.send", risk: risk, title: "Email \(id)", detail: "\(id)@example.test")
}

private func waiting(queue: [CorralRunCard.Checkpoint]) -> CorralCardModel {
    CorralCardModel(agentName: "Scout", card: CorralRunCard(
        status: .needsYou, prompt: "Tidy my inbox", elapsedMs: 0, spend: "$0.00",
        actions: [.approve, .deny, .stop], checkpoint: checkpoint("cp-1", .high), queue: queue))
}

@Test("TP-CHK-20 the queue behind the shown checkpoint decodes from the fixture Rust sends")
func queueDecodes() throws {
    let roster = try CorralRoster.decode(fixture("roster-queue-v1.json")).get()
    let card = try #require(roster.cards["scout"])
    #expect(card.checkpoint?.id == "cp-1")
    #expect(card.queue.map(\.id) == ["cp-2", "cp-3"])
    #expect(card.queue.first?.risk == .critical)
    let plain = try CorralRoster.decode(fixture("roster-checkpoint-v1.json")).get()
    #expect(plain.cards.values.allSatisfy { $0.queue.isEmpty })
}

@Test("TP-CHK-21 the box steps through the waiting checkpoints; the buttons are for the one shown")
func stepping() {
    let model = waiting(queue: [checkpoint("cp-2", .critical), checkpoint("cp-3", .normal)])
    #expect(model.stepCount == 3)
    #expect(model.step == "1 of 3")
    #expect(model.approval?.checkpointID == "cp-1")

    let second = model.showing(1)
    #expect(second.step == "2 of 3")
    #expect(second.approval?.checkpointID == "cp-2")
    #expect(second.buttons.contains(.approve(checkpoint: "cp-2", touchID: true)), "critical: Touch ID")
    #expect(second.buttons.contains(.deny(checkpoint: "cp-2")))
    #expect(!second.buttons.contains(.approve(checkpoint: "cp-1", touchID: false)))
    #expect(second.buttons.contains(.stop), "the rest stay")

    let third = model.showing(2)
    #expect(third.buttons.contains(.approve(checkpoint: "cp-3", touchID: false)))
    #expect(model.showing(9).approval?.checkpointID == "cp-3", "past the end: the last")
    #expect(model.showing(-1).approval?.checkpointID == "cp-1")
}

@Test("TP-CHK-21 the card stays on the checkpoint it showed while the others come and go")
func stepFollowsTheCheckpoint() {
    let model = waiting(queue: [checkpoint("cp-2", .critical), checkpoint("cp-3", .normal)])
    #expect(model.index(of: "cp-3") == 2)
    #expect(model.index(of: "gone") == 0, "answered elsewhere: back to the oldest")
    #expect(model.index(of: nil) == 0)
    #expect(model.stepped(from: 0, by: -1) == 0)
    #expect(model.stepped(from: 1, by: 1) == 2)
    #expect(model.stepped(from: 2, by: 1) == 2)
}

@Test("TP-CHK-21 one checkpoint has no stepper")
func oneHasNoStepper() {
    let one = waiting(queue: [])
    #expect(one.stepCount == 1 && one.step == nil)
    #expect(one.showing(1).approval?.checkpointID == "cp-1")
    let working = CorralCardModel(agentName: "Scout", card: CorralRunCard(
        status: .working, prompt: "p", elapsedMs: 0, spend: "$0.00", actions: [.stop]))
    #expect(working.stepCount == 0 && working.step == nil)
}

@Test("TP-CHK-20 the queue is bounded: valid IDs once, never the one shown")
func queueIsBounded() throws {
    let cp = { (id: String) in
        #"{"id":"\#(id)","class":"comms.send","risk":"normal","title":"t","detail":"d","detailTruncated":false}"#
    }
    let queue = (["cp-1", "cp-2", "cp-2", "has space"] + (3...20).map { "cp-\($0)" }).map(cp).joined(separator: ",")
    let body = #"{"v":1,"agents":[{"id":"scout","name":"Scout","monogram":"Sc","status":"needsYou"}],"hidden":0,"cards":[{"id":"scout","status":"needsYou","prompt":"p","promptTruncated":false,"elapsedMs":0,"spend":"$0.00","answerTruncated":false,"actions":["approve","deny"],"checkpoint":\#(cp("cp-1")),"queue":[\#(queue)]}]}"#
    let card = try #require(try CorralRoster.decode(Data(body.utf8)).get().cards["scout"])
    #expect(card.queue.first?.id == "cp-2")
    #expect(card.queue.count == CorralRunCard.maxQueue)
    #expect(Set(card.queue.map(\.id)).count == card.queue.count)
    #expect(!card.queue.contains { $0.id == "cp-1" || $0.id == "has space" })
}

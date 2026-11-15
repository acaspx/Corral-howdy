// CorralApprovalTests.swift: a checkpoint in the hover card (M2 item 4; docs/07 §8.2).
// The approval box is drawn only from the core's checkpoint; Approve and Deny go back as intents
// carrying the checkpoint's single-use ID. The fixtures are shared with the Rust shell
// (apps/desktop/src-tauri/src/bridge.rs, `checkpoint_tests`).
import Testing
import Foundation
@testable import CorralNative

private func fixture(_ name: String) throws -> Data {
    let root = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    return try Data(contentsOf: root.appendingPathComponent("Fixtures/bridge/\(name)"))
}

private func sameJSON(_ a: Data, _ b: Data) throws -> Bool {
    let x = try JSONSerialization.jsonObject(with: a) as? NSDictionary
    let y = try JSONSerialization.jsonObject(with: b) as? NSDictionary
    return x != nil && x == y
}

/// A roster with one needs-you card whose checkpoint is `checkpoint` (raw JSON).
private func waiting(_ checkpoint: String, actions: String = #"["approve","deny"]"#) -> CorralRunCard? {
    let card = #"{"id":"a","status":"needsYou","prompt":"hi","promptTruncated":false,"elapsedMs":1000,"spend":"$0.00","answerTruncated":false,"actions":\#(actions),"checkpoint":\#(checkpoint)}"#
    let json = #"{"v":1,"hidden":0,"agents":[{"id":"a","name":"A","monogram":"A","status":"needsYou"}],"cards":[\#(card)]}"#
    return try? CorralRoster.decode(Data(json.utf8)).get().cards["a"]
}

private func checkpoint(id: String = "cp-1", risk: String = "high", title: String = "Sending a message or email",
                        detail: String = "email.send to a@example.test", extra: String = "") -> String {
    let encoded = String(data: try! JSONEncoder().encode(detail), encoding: .utf8)!
    let encodedTitle = String(data: try! JSONEncoder().encode(title), encoding: .utf8)!
    return #"{"id":"\#(id)","class":"comms.send","risk":"\#(risk)","title":\#(encodedTitle),"detail":\#(encoded),"detailTruncated":false\#(extra)}"#
}

private func model(risk: String = "high", detail: String = "email.send to a@example.test") throws -> CorralCardModel {
    let card = try #require(waiting(checkpoint(risk: risk, detail: detail)))
    return CorralCardModel(agentName: "Scout", card: card)
}

@Test("TP-CHK-08 a waiting checkpoint decodes from the fixture with its approval box")
func checkpointFixtureDecodes() throws {
    let r = try CorralRoster.decode(fixture("roster-checkpoint-v1.json")).get()
    let card = try #require(r.cards["scout"])
    #expect(card.status == .needsYou)
    #expect(card.actions == [.approve, .deny])
    let cp = try #require(card.checkpoint)
    #expect(cp.id == "cp-1")
    #expect(cp.actionClass == "comms.send")
    #expect(cp.risk == .high)
    #expect(cp.title == "Sending a message or email")
    #expect(cp.detail.hasPrefix("email.send to attacker@example.test\n"))
    #expect(cp.detailTruncated == false)
}

@Test("TP-HOVER-08 approval text is cleaned and capped again; a bad ID or unknown risk drops the checkpoint and its buttons")
func checkpointSanitized() throws {
    let sneaky = "email.send to \u{202E}tset.elpmaxe@a\u{202C}\u{0007}\n" + String(repeating: "x", count: 3_000)
    let card = try #require(waiting(checkpoint(title: String(repeating: "t", count: 500), detail: sneaky)))
    let cp = try #require(card.checkpoint)
    #expect(!cp.detail.unicodeScalars.contains("\u{202E}"))
    #expect(!cp.detail.unicodeScalars.contains("\u{0007}"))
    #expect(cp.detail.contains("\n"), "line breaks stay")
    #expect(cp.detail.count == CorralRunCard.Checkpoint.maxDetailLength)
    #expect(cp.detailTruncated, "cut here, so the card says so")
    #expect(cp.title.count == CorralRunCard.Checkpoint.maxTitleLength)
    #expect((CorralRunCard.Checkpoint.maxTitleLength, CorralRunCard.Checkpoint.maxDetailLength) == (120, 2_000))

    for bad in [checkpoint(id: ""), checkpoint(id: String(repeating: "c", count: 65)),
                checkpoint(id: "cp\\u0007"), checkpoint(risk: "low"), checkpoint(risk: "")] {
        let card = try #require(waiting(bad), "the roster still decodes: \(bad)")
        #expect(card.checkpoint == nil, "never guessed at: \(bad)")
        #expect(card.actions.isEmpty, "no Approve or Deny without a checkpoint: \(bad)")
    }
    #expect(waiting(checkpoint(extra: #","args":{}"#)) == nil, "unknown keys reject the roster, like Rust")
}

@Test("TP-HOVER-02 a needs-you card shows the approval box with Approve and Deny, and its field denies with a note")
func needsYouCard() throws {
    let m = try model()
    #expect(m.status == "Needs your approval · 1s")
    let approval = try #require(m.approval)
    #expect(approval.checkpointID == "cp-1")
    #expect(approval.title == "Sending a message or email")
    #expect(approval.detail == "email.send to a@example.test")
    #expect(approval.classLabel == "comms.send")
    #expect(approval.riskLabel == "High risk")
    #expect(m.buttons == [.approve(checkpoint: "cp-1", touchID: false), .deny(checkpoint: "cp-1")])
    #expect(m.buttons.first?.isPrimary == true)
    #expect(m.buttons.map(\.title) == ["Approve", "Deny"])
    #expect(m.fieldPlaceholder == "Deny with a note for Scout")
    #expect(m.fieldIntent("  Don't email anyone.  ", agentID: "a")
            == .denyCheckpoint(id: "a", checkpoint: "cp-1", note: "Don't email anyone."))
    #expect(m.fieldIntent(" \n ", agentID: "a") == nil, "an empty note sends nothing")
    #expect(CorralCardModel.intent(for: .approve(checkpoint: "cp-1", touchID: false), agentID: "a")
            == .approveCheckpoint(id: "a", checkpoint: "cp-1", forRun: false))
    #expect(CorralCardModel.intent(for: .deny(checkpoint: "cp-1"), agentID: "a")
            == .denyCheckpoint(id: "a", checkpoint: "cp-1", note: nil))

    let working = CorralCardModel(agentName: "Scout", card: CorralRunCard(
        status: .working, prompt: "hi", elapsedMs: 0, spend: "$0.00", actions: [.stop]))
    #expect(working.approval == nil)
    #expect(working.fieldIntent("hello", agentID: "a") == nil, "a working card has no field")
}

@Test("TP-HOVER-09 high-risk Approve needs a pinned card and the whole detail; normal doesn't; critical waits for Touch ID")
func approveRules() throws {
    let normal = try #require(try model(risk: "normal").approval)
    #expect(normal.canApprove(pinned: false, expanded: false))

    let high = try #require(try model(risk: "high").approval)
    #expect(!high.needsExpanding)
    #expect(!high.canApprove(pinned: false, expanded: false), "hovering isn't enough")
    #expect(high.canApprove(pinned: true, expanded: false))
    #expect(high.approveHint(pinned: false, expanded: false) == "Click the drop to pin this card, then approve.")
    #expect(high.approveHint(pinned: true, expanded: false) == nil)

    let long = String(repeating: "line\n", count: 10)
    let highLong = try #require(try model(risk: "high", detail: long).approval)
    #expect(highLong.needsExpanding)
    #expect(!highLong.canApprove(pinned: true, expanded: false))
    #expect(highLong.approveHint(pinned: true, expanded: false) == "Show the whole action before approving.")
    #expect(highLong.canApprove(pinned: true, expanded: true))
    let wide = try #require(try model(risk: "high", detail: String(repeating: "w", count: 300)).approval)
    #expect(wide.needsExpanding, "long lines count too")

    let critical = try model(risk: "critical")
    #expect(critical.buttons.first == .approve(checkpoint: "cp-1", touchID: true))
    #expect(critical.buttons.first?.title == "Approve with Touch ID")
    let cp = try #require(critical.approval)
    #expect(cp.riskLabel == "Critical")
    #expect(!cp.canApprove(pinned: true, expanded: true), "Touch ID on the card comes next")
    #expect(cp.approveHint(pinned: true, expanded: true) == "Touch ID approval isn't ready yet. Deny, or let it wait.")
}

@Test("TP-CHK-08 Approve and Deny are encoded exactly as the fixtures Rust decodes")
func approvalIntentsMatchFixtures() throws {
    let approve = try #require(CorralIntentWire.encode(.approveCheckpoint(id: "scout", checkpoint: "cp-1", forRun: false)))
    #expect(try sameJSON(approve, fixture("intent-approve-checkpoint-v1.json")))
    let deny = try #require(CorralIntentWire.encode(.denyCheckpoint(id: "scout", checkpoint: "cp-1", note: "Don't email anyone.")))
    #expect(try sameJSON(deny, fixture("intent-deny-checkpoint-v1.json")))
    let forRun = try #require(CorralIntentWire.encode(.approveCheckpoint(id: "scout", checkpoint: "cp-1", forRun: true)))
    let object = try #require(try JSONSerialization.jsonObject(with: forRun) as? [String: Any])
    let intent = try #require(object["intent"] as? [String: Any])
    #expect(intent["scope"] as? String == "forRun")
    let bare = try #require(CorralIntentWire.encode(.denyCheckpoint(id: "scout", checkpoint: "cp-1", note: nil)))
    let bareIntent = try #require((try JSONSerialization.jsonObject(with: bare) as? [String: Any])?["intent"] as? [String: Any])
    #expect(bareIntent["note"] == nil, "no note key at all, which Rust reads as no note")
}

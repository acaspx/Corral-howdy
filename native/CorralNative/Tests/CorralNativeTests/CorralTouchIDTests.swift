// CorralTouchIDTests.swift: Approve with Touch ID on the hover card (M2 item 4, TP-CHK-18).
// A critical action's Approve asks Rust for Touch ID; Rust has Swift's prompt ask macOS (Touch ID,
// or the login password when Touch ID can't be used) and decides on the result (ADR-001). The
// card never says how the prompt went. The fixture is shared with the Rust shell
// (apps/desktop/src-tauri/src/bridge.rs, `touch_id_tests`).
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

private func card(risk: CorralRunCard.Checkpoint.Risk, detail: String = "payment.send to store.example") -> CorralCardModel {
    CorralCardModel(agentName: "Scout", card: CorralRunCard(
        status: .needsYou, prompt: "Buy the tickets.", elapsedMs: 1_000, spend: "$0.00",
        actions: [.approve, .deny],
        checkpoint: .init(id: "cp-1", actionClass: "money.spend", risk: risk,
                          title: "Spending money", detail: detail)))
}

@Test("TP-CHK-18 a critical Approve reads Approve with Touch ID and follows high risk's rules")
func touchIDRules() throws {
    let m = card(risk: .critical)
    #expect(m.buttons == [.approve(checkpoint: "cp-1", touchID: true), .deny(checkpoint: "cp-1")])
    #expect(m.buttons.first?.title == "Approve with Touch ID")
    let a = try #require(m.approval)
    #expect(!a.canApprove(pinned: false, expanded: false), "hovering isn't enough")
    #expect(a.approveHint(pinned: false, expanded: false) == "Click the drop to pin this card, then approve.")
    #expect(a.canApprove(pinned: true, expanded: false))
    #expect(a.approveHint(pinned: true, expanded: false) == nil)
    let long = try #require(card(risk: .critical, detail: String(repeating: "line\n", count: 10)).approval)
    #expect(!long.canApprove(pinned: true, expanded: false))
    #expect(long.approveHint(pinned: true, expanded: false) == "Show the whole action before approving.")
    #expect(long.canApprove(pinned: true, expanded: true))
    #expect(a.forRunTitle == nil, "Touch ID every time")
}

@Test("TP-CHK-18 Approve with Touch ID asks Rust, encoded exactly as the fixture Rust decodes")
func touchIDIntent() throws {
    let intent = CorralCardModel.intent(for: .approve(checkpoint: "cp-1", touchID: true), agentID: "scout")
    #expect(intent == .approveWithTouchId(id: "scout", checkpoint: "cp-1"))
    let data = try #require(CorralIntentWire.encode(intent))
    #expect(try sameJSON(data, fixture("intent-approve-touch-id-v1.json")))
    // A plain Approve is unchanged.
    #expect(CorralCardModel.intent(for: .approve(checkpoint: "cp-1", touchID: false), agentID: "scout")
            == .approveCheckpoint(id: "scout", checkpoint: "cp-1", forRun: false))
}

@Test("TP-CHK-18 a critical action can't be edited on the card yet")
func touchIDNoEdit() throws {
    let a = try #require(card(risk: .critical).approval)
    #expect(a.editHint(["to": "x"], pinned: true) == "A critical action can't be edited yet. Approve it with Touch ID, or deny.")
}

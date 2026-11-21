// CorralIdleTests.swift: Swift tells Rust when the corral is idle, so a new usage order is applied
// only when nothing can be under the pointer (TP-USAGE-02, 10, 11). Swift only reports; Rust
// decides whether the order moves.
import Testing
import Foundation
@testable import CorralNative

private func fixture(_ name: String) throws -> Data {
    let root = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    return try Data(contentsOf: root.appendingPathComponent("Fixtures/bridge/\(name)"))
}

@Test("TP-USAGE-10 the idle report encodes as the fixture Rust decodes")
func idleMatchesFixture() throws {
    let sent = try #require(CorralIntentWire.encode(.corralIdle(true)))
    let a = try JSONSerialization.jsonObject(with: sent) as? NSDictionary
    let b = try JSONSerialization.jsonObject(with: fixture("intent-corral-idle-v1.json")) as? NSDictionary
    #expect(a != nil && a == b)
    let busy = try #require(CorralIntentWire.encode(.corralIdle(false)))
    let c = try JSONSerialization.jsonObject(with: busy) as? NSDictionary
    #expect(c?["intent"] as? NSDictionary == ["kind": "corralIdle", "idle": false] as NSDictionary)
}

@Test("TP-USAGE-11 the corral is idle with no hover, no card opening and no card open")
func hoverStateIdle() {
    var s = CorralHoverState()
    #expect(s.isIdle)
    s.pointer(overDrop: "scout", at: 0)
    #expect(!s.isIdle, "hovering a drop")
    s.pointer(overDrop: nil, at: 0.05)
    #expect(s.isIdle, "left before the card opened")

    s.pointer(overDrop: "scout", at: 1)
    s.tick(at: 1 + CorralHoverState.openDelay)
    #expect(s.shown == "scout")
    s.pointer(overDrop: nil, at: 2)
    #expect(!s.isIdle, "the card is still open")
    s.pointer(overCard: true, at: 2.1)
    #expect(!s.isIdle, "on the card")
    s.pointer(overCard: false, at: 2.2)
    s.tick(at: 2.2 + CorralHoverState.closeDelay)
    #expect(s.shown == nil)
    #expect(s.isIdle, "the card closed")

    s.click(drop: "scout")
    #expect(!s.isIdle, "a pinned card")
    s.dismiss()
    #expect(s.isIdle)
}

@Test("TP-USAGE-11 only changes are reported, starting with the first state")
func reporterSendsChanges() {
    var r = CorralIdleReporter()
    #expect(r.report(true) == true, "the first state is always sent")
    #expect(r.report(true) == nil)
    #expect(r.report(false) == false)
    #expect(r.report(false) == nil)
    #expect(r.report(true) == true)
}

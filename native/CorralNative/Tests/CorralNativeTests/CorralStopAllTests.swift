// CorralStopAllTests.swift: "Stop all agents" in the corral's right-click menu (TP-SUP-02, the
// Recall-all entry point in docs/07 §6). Swift only offers the item and reports the click; Rust
// stops the runs.
import Testing
import Foundation
@testable import CorralNative

private func fixture(_ name: String) throws -> Data {
    let root = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    return try Data(contentsOf: root.appendingPathComponent("Fixtures/bridge/\(name)"))
}

@Test("TP-SUP-02 Stop all encodes as the fixture Rust decodes")
func stopAllMatchesFixture() throws {
    let sent = try #require(CorralIntentWire.encode(.stopAll))
    let a = try JSONSerialization.jsonObject(with: sent) as? NSDictionary
    let b = try JSONSerialization.jsonObject(with: fixture("intent-stop-all-v1.json")) as? NSDictionary
    #expect(a != nil && a == b)
}

@Test("TP-SUP-02 Stop all is offered only while an agent is working or waiting on you")
func stopAllOnlyWhenSomethingRuns() {
    func token(_ id: String, _ status: CorralAgentToken.Status) -> CorralAgentToken {
        CorralAgentToken(id: id, name: id, monogram: "A", status: status)
    }
    #expect(CorralStopAllMenu.isAvailable(agents: []) == false)
    #expect(CorralStopAllMenu.isAvailable(agents: [token("a", .available), token("b", .done), token("c", .error)]) == false)
    #expect(CorralStopAllMenu.isAvailable(agents: [token("a", .available), token("b", .working)]))
    #expect(CorralStopAllMenu.isAvailable(agents: [token("a", .needsYou)]))
}

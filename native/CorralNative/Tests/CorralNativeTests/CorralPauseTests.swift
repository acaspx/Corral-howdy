// CorralPauseTests.swift: Pause and Resume on the card, Swift side (TP-PAUSE-05). The core
// decides which runs can pause and sends Pausing and Paused; Swift draws them and sends the
// buttons back as intents. Fixtures are shared with Rust (bridge.rs, `pause_tests`).
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

private func card(_ status: CorralRunCard.Status, _ actions: [CorralRunCard.Action]) -> CorralRunCard {
    CorralRunCard(status: status, prompt: "Tidy my notes.", elapsedMs: 3_000, spend: "$0.01",
                  actions: actions)
}

@Test("TP-PAUSE-05 a paused drop, and pausing and paused cards, decode as the core sent them")
func pausedRosterDecodes() throws {
    let r = try CorralRoster.decode(fixture("roster-paused-v1.json")).get()
    #expect(r.agents.map(\.status) == [.paused, .working, .working])
    #expect(r.cards["scout"]?.status == .paused)
    #expect(r.cards["scout"]?.actions == [.resume, .stop])
    #expect(r.cards["ranger"]?.status == .pausing)
    #expect(r.cards["ranger"]?.actions == [.resume, .stop])
    #expect(r.cards["drover"]?.status == .working)
    #expect(r.cards["drover"]?.actions == [.pause, .stop])
}

@Test("TP-PAUSE-05 the card reads Pausing, then Paused, with Resume first")
func pausedCardReads() {
    let pausing = CorralCardModel(agentName: "Scout", card: card(.pausing, [.resume, .stop]))
    #expect(pausing.status == "Pausing after this step · 3s")
    #expect(pausing.buttons == [.resume, .stop])
    let paused = CorralCardModel(agentName: "Scout", card: card(.paused, [.resume, .stop]))
    #expect(paused.status == "Paused · 3s · $0.01")
    #expect(paused.buttons.map(\.title) == ["Resume", "Stop"])
    #expect(paused.buttons.first?.isPrimary == true)
    let working = CorralCardModel(agentName: "Scout", card: card(.working, [.pause, .stop]))
    #expect(working.buttons.map(\.title) == ["Pause", "Stop"])
}

@Test("TP-PAUSE-05 Pause and Resume are sent exactly as the fixtures Rust decodes")
func pauseIntentsMatchFixtures() throws {
    #expect(CorralCardModel.intent(for: .pause, agentID: "scout") == .pause(id: "scout"))
    #expect(CorralCardModel.intent(for: .resume, agentID: "scout") == .resume(id: "scout"))
    for (intent, file) in [(CorralIntent.pause(id: "scout"), "intent-pause-v1.json"),
                           (CorralIntent.resume(id: "scout"), "intent-resume-v1.json")] {
        let encoded = try #require(CorralIntentWire.encode(intent))
        #expect(try sameJSON(encoded, fixture(file)), "\(intent) does not match \(file)")
    }
}

@Test("TP-PAUSE-05 a paused drop holds its arc still with a pause badge, never color alone")
func pausedMark() {
    #expect(CorralStatusMark.of(.paused) == .stillArc(badge: "pause.fill"))
    #expect(CorralStatusMark.of(.working) == .turningArc, "working still turns")
}

@Test("TP-PAUSE-05 the trace tree names a paused or pausing column")
func pausedColumns() {
    #expect(CorralTraceColumnView.statusText(.paused) == "Paused")
    #expect(CorralTraceColumnView.statusText(.pausing) == "Pausing")
    #expect(CorralTrace.Status(rawValue: "paused") == .paused)
    #expect(CorralTrace.Status(rawValue: "pausing") == .pausing)
}

@Test("TP-PAUSE-05 Stop all is offered while an agent is paused")
func stopAllWhilePaused() {
    let paused = CorralAgentToken(id: "a", name: "a", monogram: "A", status: .paused)
    #expect(CorralStopAllMenu.isAvailable(agents: [paused]))
}

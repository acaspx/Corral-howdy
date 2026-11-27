// CorralJourneyTests.swift: end-to-end journeys, Swift side (Spike S2, docs/04-test-plan.md §6).
// Each transcript in Fixtures/journeys is recorded by the Rust core playing the journey
// (apps/desktop/src-tauri/tests/journeys.rs): the gestures, the intent each must send, the
// rosters the core sent back, and what the corral should show after each step. This replays it
// into CorralNative's real decoding and models: every roster must decode, each gesture performed
// on the corral's own card model must send exactly the recorded intent, and the shape, the drops
// in slot order with their status, and the trace columns must be what the step expects.
import Testing
import Foundation
@testable import CorralNative

private enum ReplayError: Error {
    case unknownGesture(String)
}

private func transcript(_ journey: String) throws -> [String: Any] {
    // Tests/CorralNativeTests/<this file> → package root → Fixtures/journeys/<journey>.json
    let root = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    let data = try Data(contentsOf: root.appendingPathComponent("Fixtures/journeys/\(journey).json"))
    return try #require(try JSONSerialization.jsonObject(with: data) as? [String: Any])
}

/// The shape a transcript names, in the corral's grammar (docs/07 §3).
private func shape(named name: String) -> CorralShape? {
    switch name {
    case "empty": .empty
    case "circle": .circle
    case "dumbbell": .dumbbell
    case "triangle": .polygon(sides: 3)
    case "square": .polygon(sides: 4)
    case "pentagon": .polygon(sides: 5)
    case "hexagon": .polygon(sides: 6)
    case "heptagon": .polygon(sides: 7)
    case "octagon": .polygon(sides: 8)
    default: nil
    }
}

/// A drop's status as the wire names it.
private func wireName(_ status: CorralAgentToken.Status) -> String {
    switch status {
    case .available: "available"
    case .working: "working"
    case .paused: "paused"
    case .needsYou: "needsYou"
    case .done: "done"
    case .error: "error"
    }
}

/// A card button a transcript names.
private func button(named name: String) -> CorralCardModel.Button? {
    switch name {
    case "stop": .stop
    case "pause": .pause
    case "resume": .resume
    case "sendBack": .sendBack
    case "retry": .retry
    default: nil
    }
}

private func json(_ object: Any) throws -> NSDictionary? {
    let data = try JSONSerialization.data(withJSONObject: object)
    return try JSONSerialization.jsonObject(with: data) as? NSDictionary
}

/// What the user's gesture sends, performed on the card model the corral builds for that drop.
private func perform(_ gesture: [String: Any], on roster: CorralRoster) throws -> CorralIntent {
    let agent = try #require(gesture["agent"] as? String)
    let token = try #require(roster.agents.first { $0.id == agent }, "\(agent) isn't in the corral")
    let model = CorralCardModel(agentName: token.name, card: roster.cards[agent], linkedApp: token.linkedApp)
    switch gesture["kind"] as? String {
    case "task":
        let text = try #require(gesture["text"] as? String)
        return try #require(model.fieldIntent(text, agentID: agent), "\(agent)'s card takes no task")
    case "press":
        let name = try #require(gesture["button"] as? String)
        let pressed = try #require(button(named: name), "no button \(name)")
        try #require(model.buttons.contains(pressed), "\(agent)'s card has no \(name) button")
        return CorralCardModel.intent(for: pressed, agentID: agent)
    default:
        throw ReplayError.unknownGesture(String(describing: gesture["kind"]))
    }
}

/// Replays `journey`: every step's gesture, rosters and expectations.
private func replay(_ journey: String) throws {
    let recorded = try transcript(journey)
    let steps = try #require(recorded["steps"] as? [[String: Any]])
    #expect(!steps.isEmpty)
    var current: CorralRoster?
    for step in steps {
        let say = step["say"] as? String ?? "?"
        if let gesture = step["gesture"] as? [String: Any] {
            let roster = try #require(current, "\(say): a gesture before the corral opened")
            let intent = try perform(gesture, on: roster)
            let sent = try #require(CorralIntentWire.encode(intent))
            let expected = try #require(step["intent"] as? [String: Any], "\(say): no intent recorded")
            #expect(try JSONSerialization.jsonObject(with: sent) as? NSDictionary == json(expected),
                    "\(say): the gesture sends the recorded intent")
        }
        let rosters = try #require(step["rosters"] as? [Any])
        #expect(!rosters.isEmpty, "\(say): the core sent a roster")
        for roster in rosters {
            let data = try JSONSerialization.data(withJSONObject: roster)
            switch CorralRoster.decode(data) {
            case .success(let decoded): current = decoded
            case .failure(let error): Issue.record("\(say): a roster didn't decode: \(error)")
            }
        }
        let shown = try #require(current)
        let expect = try #require(step["expect"] as? [String: Any])

        let named = try #require(expect["shape"] as? String)
        let wanted = try #require(shape(named: named), "\(say): no shape \(named)")
        #expect(CorralLayout.shape(forShownCount: shown.agents.count) == wanted, "\(say): the shape")
        let layout = CorralLayout.layout(rankedIDs: shown.agents.map(\.id))
        #expect(layout.shape == wanted, "\(say): the layout's shape")

        let drops = try #require(expect["drops"] as? [[String: String]])
        #expect(shown.agents.map(\.id) == drops.compactMap { $0["id"] }, "\(say): the drops in slot order")
        #expect(shown.agents.map { wireName($0.status) } == drops.compactMap { $0["status"] },
                "\(say): each drop's status")
        let ranked = layout.blobs.filter { $0.rank > 0 }.sorted { $0.rank < $1.rank }.map(\.id)
        #expect(ranked == drops.compactMap { $0["id"] }, "\(say): rank 1 first, clockwise")

        let columns = try #require(expect["columns"] as? [String])
        #expect(shown.trace.columns.map(\.id) == columns, "\(say): the trace columns")
    }
}

// MARK: J9 the corral morphs

@Test("J9 the corral morphs as agents come and go: triangle, square, triangle, and back to ready")
func j9CorralMorph() throws {
    try replay("J9")
}

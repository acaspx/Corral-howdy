// CorralBridgeTests.swift: TP-BRIDGE-01..03, Swift side (docs/04-test-plan.md). Spike S1.
// The fixtures in Fixtures/bridge are the wire contract; the Rust shell tests the same files
// (apps/desktop/src-tauri/src/bridge.rs), so a change on either side fails the other's tests.
import Testing
import Foundation
@testable import CorralNative

private func fixture(_ name: String) throws -> Data {
    // Tests/CorralNativeTests/<this file> → package root → Fixtures/bridge/<name>
    let root = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    return try Data(contentsOf: root.appendingPathComponent("Fixtures/bridge/\(name)"))
}

private func roster(_ json: String) -> Result<CorralRoster, CorralRoster.DecodeError> {
    CorralRoster.decode(Data(json.utf8))
}

private func agentsJSON(_ ids: [String], status: String = "available") -> String {
    ids.map { #"{"id":"\#($0)","name":"\#($0)","monogram":"A","status":"\#(status)"}"# }
        .joined(separator: ",")
}

/// Compares two JSON documents as values, so key order and whitespace don't matter.
private func sameJSON(_ a: Data, _ b: Data) throws -> Bool {
    let x = try JSONSerialization.jsonObject(with: a) as? NSDictionary
    let y = try JSONSerialization.jsonObject(with: b) as? NSDictionary
    return x != nil && x == y
}

// MARK: TP-BRIDGE-01

@Test("TP-BRIDGE-01 a roster from Rust keeps its drop IDs, order and statuses")
func rosterFixtureDecodesInOrder() throws {
    let r = try CorralRoster.decode(fixture("roster-v1.json")).get()
    #expect(r.agents.map(\.id) == ["scout", "ranger", "drover"])
    #expect(r.agents.map(\.status) == [.available, .working, .needsYou])
    #expect(r.agents.map(\.monogram) == ["Sc", "Rg", "Dv"])
    #expect(r.hiddenCount == 0)
    // The layout receives the same IDs in the same order (rank 1 top-left, then clockwise).
    #expect(CorralLayout.layout(rankedIDs: r.agents.map(\.id)).blobs.map(\.id) == ["scout", "ranger", "drover"])
}

// MARK: TP-BRIDGE-02 (Swift side: what Swift sends)

@Test("TP-BRIDGE-02 intents are encoded exactly as the fixtures Rust decodes")
func intentsMatchFixtures() throws {
    let cases: [(CorralIntent, String)] = [
        (.openAgent(id: "scout"), "intent-open-agent-v1.json"),
        (.deploy(agentIDs: ["scout", "drover"]), "intent-deploy-v1.json"),
        (.summonPromptBar, "intent-summon-prompt-bar-v1.json"),
    ]
    for (intent, file) in cases {
        let encoded = try #require(CorralIntentWire.encode(intent))
        #expect(try sameJSON(encoded, fixture(file)), "\(intent) does not match \(file)")
    }
}

// MARK: TP-BRIDGE-03

@Test("TP-BRIDGE-03 duplicate IDs are dropped, keeping the first")
func duplicatesDropped() throws {
    let r = try roster(#"{"v":1,"hidden":0,"agents":[\#(agentsJSON(["a", "b", "a", "c", "b"]))]}"#).get()
    #expect(r.agents.map(\.id) == ["a", "b", "c"])
}

@Test("TP-BRIDGE-03 more than 8 agents: the first 8 show, the rest count as hidden")
func boundedToEight() throws {
    let ids = (1...11).map { "agent-\($0)" }
    let r = try roster(#"{"v":1,"hidden":2,"agents":[\#(agentsJSON(ids))]}"#).get()
    #expect(r.agents.map(\.id) == Array(ids.prefix(8)))
    #expect(r.hiddenCount == 5)
}

@Test("TP-BRIDGE-03 names and monograms are stripped of control characters and truncated")
func textSanitized() throws {
    let longName = String(repeating: "x", count: 500)
    let json = #"{"v":1,"hidden":0,"agents":[{"id":"a","name":"Ev\u0000il‮\n\#(longName)","monogram":"\u0007ABCDEF","status":"done"}]}"#
    let a = try #require(try roster(json).get().agents.first)
    #expect(!a.name.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) })
    #expect(a.name.hasPrefix("Evil"))
    #expect(a.name.count == CorralRoster.maxNameLength)
    #expect(a.monogram == "ABC")
}

@Test("TP-BRIDGE-03 bad IDs (empty, too long, control characters) drop that agent only")
func badIDsDropped() throws {
    let long = String(repeating: "i", count: CorralRoster.maxIDLength + 1)
    let json = #"{"v":1,"hidden":0,"agents":[\#(agentsJSON(["", long, "ok"])),{"id":"bad\u0000id","name":"n","monogram":"m","status":"done"}]}"#
    #expect(try roster(json).get().agents.map(\.id) == ["ok"])
}

@Test("TP-BRIDGE-03 a negative or huge hidden count is clamped")
func hiddenClamped() throws {
    #expect(try roster(#"{"v":1,"hidden":-4,"agents":[]}"#).get().hiddenCount == 0)
    #expect(try roster(#"{"v":1,"hidden":999999999,"agents":[]}"#).get().hiddenCount == CorralRoster.maxHidden)
}

@Test("TP-BRIDGE-03 an empty monogram falls back to the name's first letter")
func monogramFallback() throws {
    let r = try roster(#"{"v":1,"hidden":0,"agents":[{"id":"a","name":"wrangler","monogram":" \u0000 ","status":"done"}]}"#).get()
    #expect(r.agents.first?.monogram == "W")
}

@Test("TP-BRIDGE-03 malformed, oversized, wrong-version or unknown-status rosters are rejected, never trapped on")
func rejectedRosters() {
    #expect(CorralRoster.decode(Data()) == .failure(.empty))
    #expect(roster("{") == .failure(.malformed))
    #expect(roster("[]") == .failure(.malformed))
    #expect(roster(#"{"v":1,"agents":[]}"#) == .failure(.malformed))                 // hidden missing
    #expect(roster(#"{"v":1,"hidden":0,"agents":[],"extra":1}"#) == .failure(.malformed))
    #expect(roster(#"{"v":2,"hidden":0,"agents":[]}"#) == .failure(.unsupportedVersion(2)))
    #expect(roster(#"{"v":1,"hidden":0,"agents":[\#(agentsJSON(["a"], status: "sleeping"))]}"#)
            == .failure(.unknownStatus("sleeping")))
    let big = Data(repeating: UInt8(ascii: " "), count: CorralRoster.maxBytes + 1)
    #expect(CorralRoster.decode(big) == .failure(.tooLarge(CorralRoster.maxBytes + 1)))
    // Invalid UTF-8 never traps.
    #expect(CorralRoster.decode(Data([0xff, 0xfe, 0x7b])) == .failure(.malformed))
}

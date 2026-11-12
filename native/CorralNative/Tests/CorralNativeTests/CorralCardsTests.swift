// CorralCardsTests.swift: TP-SKEL-03..05, Swift side of the wire (docs/04-test-plan.md, SK-6a).
// Run cards ride on the roster; tasks, Stop, Send back and Retry go back as intents. The fixtures
// in Fixtures/bridge are shared with the Rust shell (apps/desktop/src-tauri/src/bridge.rs).
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

private func roster(cards: String, agents: [String] = ["a"]) -> Result<CorralRoster, CorralRoster.DecodeError> {
    let list = agents.map { #"{"id":"\#($0)","name":"\#($0)","monogram":"A","status":"working"}"# }
        .joined(separator: ",")
    return CorralRoster.decode(Data(#"{"v":1,"hidden":0,"agents":[\#(list)],"cards":[\#(cards)]}"#.utf8))
}

private func card(_ id: String = "a", status: String = "working", extra: String = "",
                  actions: String = #"["stop"]"#) -> String {
    #"{"id":"\#(id)","status":"\#(status)","prompt":"hi","promptTruncated":false,"elapsedMs":1000,"spend":"$0.00","answerTruncated":false,"actions":\#(actions)\#(extra)}"#
}

@Test("TP-SKEL-04 a roster with run cards decodes each drop's card")
func cardsFixtureDecodes() throws {
    let r = try CorralRoster.decode(fixture("roster-cards-v1.json")).get()
    #expect(r.agents.map(\.id) == ["scout", "ranger", "drover"])
    let scout = try #require(r.cards["scout"])
    #expect(scout.status == .working)
    #expect(scout.prompt == "What is the capital of France?")
    #expect(scout.elapsedMs == 3000)
    #expect(scout.spend == "$0.00")
    #expect(scout.answer == nil)
    #expect(scout.actions == [.stop])
    let ranger = try #require(r.cards["ranger"])
    #expect(ranger.status == .done)
    #expect(ranger.answer == "Hello! How can I help?")
    #expect(ranger.actions == [.sendBack, .newTask])
    let drover = try #require(r.cards["drover"])
    #expect(drover.status == .error)
    #expect(drover.error == "Add your xAI key in Settings.")
    #expect(drover.actions == [.retry, .sendBack])
}

@Test("TP-SKEL-04 a roster without cards still decodes, with no cards")
func rosterWithoutCards() throws {
    let r = try CorralRoster.decode(fixture("roster-v1.json")).get()
    #expect(r.cards.isEmpty)
}

@Test("TP-SKEL-05 card text is stripped of control and bidi characters and capped again")
func cardTextSanitized() throws {
    let long = String(repeating: "é", count: CorralRunCard.maxAnswerLength + 50)
    let extra = #","answer":"Pa‮ris\u0007\n\#(long)","error":"bad⁦ key""#
    let r = try roster(cards: card(status: "done", extra: extra, actions: #"["sendBack"]"#)).get()
    let c = try #require(r.cards["a"])
    let answer = try #require(c.answer)
    #expect(answer.hasPrefix("Paris\n"), "newlines stay, controls go: \(answer.prefix(8))")
    #expect(answer.count == CorralRunCard.maxAnswerLength)
    #expect(c.answerTruncated, "a cut made here is shown as a cut")
    #expect(c.error == "bad key")
}

@Test("TP-SKEL-04 a card for an agent that isn't shown, a duplicate, or an unknown status is dropped")
func cardsBounded() throws {
    let r = try roster(cards: [card("a"), card("a", status: "done"), card("ghost"), card("b", status: "sleeping")]
        .joined(separator: ","), agents: ["a", "b"]).get()
    #expect(Array(r.cards.keys) == ["a"])
    #expect(r.cards["a"]?.status == .working, "the first card for an agent wins")
}

@Test("TP-SKEL-04 unknown actions are dropped, not guessed at; negative times read as zero")
func cardActionsAndTime() throws {
    let r = try roster(cards: #"{"id":"a","status":"working","prompt":"hi","promptTruncated":false,"elapsedMs":-5,"spend":"$0.00","answerTruncated":false,"actions":["stop","approveAll","stop"]}"#).get()
    #expect(r.cards["a"]?.actions == [.stop])
    #expect(r.cards["a"]?.elapsedMs == 0)
}

@Test("TP-SKEL-04 a card with an unknown field rejects the roster, like Rust's deny_unknown_fields")
func cardUnknownField() {
    #expect(roster(cards: card(extra: #","model":"x""#)) == .failure(.malformed))
}

@Test("TP-SKEL-03 tasks, Stop, Send back and Retry are encoded exactly as the fixtures Rust decodes")
func taskIntentsMatchFixtures() throws {
    let cases: [(CorralIntent, String)] = [
        (.deployTask(id: "scout", prompt: "What is the capital of France?"), "intent-deploy-task-v1.json"),
        (.stop(id: "scout"), "intent-stop-v1.json"),
        (.sendBack(id: "scout"), "intent-send-back-v1.json"),
        (.retry(id: "scout"), "intent-retry-v1.json"),
    ]
    for (intent, file) in cases {
        let encoded = try #require(CorralIntentWire.encode(intent))
        #expect(try sameJSON(encoded, fixture(file)), "\(intent) does not match \(file)")
    }
}

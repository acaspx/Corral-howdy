// CorralPromptBarTests.swift: the prompt bar's wire (TP-BAR-08, docs/07 §6.1). Swift reports each
// edit and send; Rust answers with what the bar shows and how a send went. Swift decodes that
// bounded, and drops an answer older than its latest edit.
import Testing
import Foundation
@testable import CorralNative

private func fixture(_ name: String) throws -> Data {
    let root = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    return try Data(contentsOf: root.appendingPathComponent("Fixtures/bridge/\(name)"))
}

private func json(_ data: Data) throws -> NSDictionary? {
    try JSONSerialization.jsonObject(with: data) as? NSDictionary
}

private func snapshot(_ body: String) -> Result<CorralPromptBarSnapshot, CorralPromptBarSnapshot.DecodeError> {
    CorralPromptBarSnapshot.decode(Data(body.utf8))
}

private let plainView = #"{"chips":[],"placeholder":"p","button":{"label":"Deploy all (1)","enabled":true,"all":true},"error":null,"suggestions":[]}"#

@Test("TP-BAR-08 an edit and a send encode as the fixtures Rust decodes")
func barIntentsMatchFixtures() throws {
    let edited = try #require(CorralIntentWire.encode(
        .promptBarEdited(seq: 7, text: "@cl plan the trip", cursor: 3, selection: ["grok"])))
    #expect(try json(edited) == json(fixture("intent-prompt-bar-edited-v1.json")))
    let send = try #require(CorralIntentWire.encode(
        .promptBarSend(seq: 8, text: "@claude @grok plan the trip", selection: [], all: false)))
    #expect(try json(send) == json(fixture("intent-prompt-bar-send-v1.json")))
}

@Test("TP-BAR-08 the bar's view and a send's note decode from the fixtures Rust sends")
func barViewDecodes() throws {
    let shown = try CorralPromptBarSnapshot.decode(fixture("prompt-bar-v1.json")).get()
    #expect(shown.seq == 7)
    #expect(shown.view.chips == ["Grok"])
    #expect(shown.view.placeholder == "Give Grok a task")
    #expect(shown.view.button == CorralPromptBarView.Button(label: "Deploy", enabled: true, all: false))
    #expect(shown.view.error == nil)
    #expect(shown.view.suggestions == [
        CorralPromptBarView.Suggestion(id: "claude", name: "Claude", shown: true, status: .ready, completion: "@Claude "),
    ])
    #expect(shown.outcome == nil)

    let noted = try CorralPromptBarSnapshot.decode(fixture("prompt-bar-note-v1.json")).get()
    #expect(noted.outcome == .note("Sent to Claude. Grok is busy."))
    #expect(noted.view.button.all)

    let sent = try snapshot(#"{"v":1,"seq":2,"view":\#(plainView),"outcome":{"kind":"sent"}}"#).get()
    #expect(sent.outcome == .sent)
    let refused = try snapshot(#"{"v":1,"seq":2,"view":\#(plainView),"outcome":{"kind":"refused","text":"Grok is busy."}}"#).get()
    #expect(refused.outcome == .refused("Grok is busy."))
}

@Test("TP-BAR-08 a bad answer is refused whole, and never traps")
func barViewRefusesBadAnswers() {
    let bad = [
        "",
        "not json",
        #"{"v":2,"seq":1,"view":\#(plainView)}"#,
        #"{"v":1,"seq":1,"view":\#(plainView),"extra":1}"#,
        #"{"v":1,"seq":-1,"view":\#(plainView)}"#,
        #"{"v":1,"seq":1,"view":\#(plainView),"outcome":{"kind":"maybe"}}"#,
        #"{"v":1,"seq":1,"view":\#(plainView),"outcome":{"kind":"note"}}"#,
        #"{"v":1,"seq":1,"view":{"chips":[],"placeholder":"p","button":{"label":"x","enabled":true,"all":true},"error":null,"suggestions":[{"id":"a","name":"A","shown":true,"status":"asleep","completion":"@A "}]}}"#,
    ]
    for body in bad {
        guard case .failure = snapshot(body) else {
            Issue.record("accepted: \(body.prefix(80))")
            continue
        }
    }
    let huge = Data(repeating: 0x20, count: CorralPromptBarSnapshot.maxBytes + 1)
    #expect(CorralPromptBarSnapshot.decode(huge) == .failure(.tooLarge(huge.count)))
}

@Test("TP-BAR-08 what Rust sends is bounded: chips, rows, IDs and control characters")
func barViewIsBounded() throws {
    let chips = (0..<12).map { "\"A\($0)\"" }.joined(separator: ",")
    let rows = (0..<12).map {
        #"{"id":"a\#($0)","name":"A\#($0)","shown":false,"status":"working","completion":"@A\#($0) "}"#
    }
    let dup = #"{"id":"a0","name":"Again","shown":true,"status":"ready","completion":"@Again "}"#
    let badID = #"{"id":"has space","name":"Bad","shown":true,"status":"ready","completion":"@Bad "}"#
    let body = #"{"v":1,"seq":1,"view":{"chips":[\#(chips)],"placeholder":"Give ‮Grok a task","button":{"label":"Deploy","enabled":true,"all":false},"error":"No agent is called x\u0000.","suggestions":[\#(([dup.replacingOccurrences(of: "a0", with: "z")] + rows + [dup, badID]).joined(separator: ","))]}}"#
    let view = try snapshot(body).get().view
    #expect(view.chips.count == CorralPromptBarView.maxChips)
    #expect(view.suggestions.count == CorralPromptBarView.maxSuggestions)
    #expect(Set(view.suggestions.map(\.id)).count == view.suggestions.count, "each agent once")
    #expect(!view.suggestions.contains { $0.id == "has space" })
    #expect(view.placeholder == "Give Grok a task", "bidi controls stripped")
    #expect(view.error == "No agent is called x.")
}

@Test("TP-BAR-08 an answer older than the latest edit is dropped")
@MainActor func staleAnswersAreDropped() throws {
    let bar = CorralPromptBar()
    let first = bar.edited(text: "@c", utf16Cursor: 2, selection: [])
    let second = bar.send(text: "@claude go", selection: ["grok", "grok"], all: false)
    guard case .promptBarEdited(let seq1, "@c", 2, []) = first,
          case .promptBarSend(let seq2, "@claude go", ["grok"], false) = second else {
        Issue.record("intents: \(first) \(second)")
        return
    }
    #expect(seq2 == seq1 &+ 1)
    let view = try snapshot(#"{"v":1,"seq":1,"view":\#(plainView)}"#).get().view
    #expect(!bar.receive(CorralPromptBarSnapshot(seq: seq1, view: view, outcome: nil)))
    #expect(bar.view == nil)
    #expect(bar.receive(CorralPromptBarSnapshot(seq: seq2, view: view, outcome: .sent)))
    #expect(bar.view == view && bar.outcome == .sent)
}

@Test("TP-BAR-08 the cursor goes to Rust as a UTF-8 offset, on a character")
func cursorIsUTF8() {
    let text = "@é😀x" // UTF-16: 1, 1, 2, 1. UTF-8: 1, 2, 4, 1.
    let at = { CorralPromptBar.utf8Cursor(in: text, utf16: $0) }
    #expect(at(0) == 0)
    #expect(at(1) == 1)
    #expect(at(2) == 3)
    #expect(at(3) == 3, "inside the emoji: back to its start")
    #expect(at(4) == 7)
    #expect(at(5) == 8)
    #expect(at(99) == 8)
    #expect(at(-1) == 0)
}

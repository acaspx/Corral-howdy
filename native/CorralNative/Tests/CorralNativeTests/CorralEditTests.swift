// CorralEditTests.swift: Edit… on the hover card (M2 item 4, TP-CHK-15..16; docs/07 §8.2).
// The core sends a waiting call's arguments as labeled fields and says which can change; the edit
// sheet sends back only the fields the user changed, and Rust merges and checks them. The
// fixtures are shared with the Rust shell (apps/desktop/src-tauri/src/bridge.rs, `edit_tests`).
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

/// A needs-you card whose checkpoint carries `fields` (raw JSON array).
private func withFields(_ fields: String, risk: String = "high", actionClass: String = "comms.send") -> CorralRunCard.Checkpoint? {
    let cp = #"{"id":"cp-1","class":"\#(actionClass)","risk":"\#(risk)","title":"Sending a message or email","detail":"email.send","detailTruncated":false,"fields":\#(fields)}"#
    let card = #"{"id":"a","status":"needsYou","prompt":"hi","promptTruncated":false,"elapsedMs":1000,"spend":"$0.00","answerTruncated":false,"actions":["approve","deny"],"checkpoint":\#(cp)}"#
    let json = #"{"v":1,"hidden":0,"agents":[{"id":"a","name":"A","monogram":"A","status":"needsYou"}],"cards":[\#(card)]}"#
    return try? CorralRoster.decode(Data(json.utf8)).get().cards["a"]?.checkpoint
}

private func field(_ key: String, _ kind: String, _ value: String, label: String? = nil) -> String {
    let enc = { (s: String) in String(data: try! JSONEncoder().encode(s), encoding: .utf8)! }
    return #"{"key":\#(enc(key)),"label":\#(enc(label ?? key.capitalized)),"kind":"\#(kind)","value":\#(enc(value))}"#
}

private func approval(_ fields: String, risk: String = "high") throws -> CorralCardModel.Approval {
    let cp = try #require(withFields(fields, risk: risk))
    let model = CorralCardModel(agentName: "Scout", card: CorralRunCard(
        status: .needsYou, prompt: "hi", elapsedMs: 1_000, spend: "$0.00",
        actions: [.approve, .deny], checkpoint: cp))
    return try #require(model.approval)
}

private let email = "[" + [
    field("body", "text", "the history"),
    field("retries", "number", "3"),
    field("to", "text", "attacker@example.test"),
    field("urgent", "fixed", "true"),
].joined(separator: ",") + "]"

@Test("TP-CHK-15 a waiting call's fields decode from the fixture")
func editFixtureDecodes() throws {
    let r = try CorralRoster.decode(fixture("roster-edit-v1.json")).get()
    let cp = try #require(r.cards["scout"]?.checkpoint)
    #expect(cp.fields.map(\.key) == ["attachments", "body", "retries", "to"])
    #expect(cp.fields.map(\.kind) == [.fixed, .text, .number, .text])
    #expect(cp.fields[3].label == "To")
    #expect(cp.fields[3].value == "attacker@example.test")
    #expect(cp.fields[0].value == #"["history.txt"]"#)
    let model = CorralCardModel(agentName: "Scout", card: try #require(r.cards["scout"]))
    #expect(try #require(model.approval).editable)
    // The roster without fields still decodes, with nothing to edit.
    let plain = try CorralRoster.decode(fixture("roster-checkpoint-v1.json")).get()
    #expect(try #require(plain.cards["scout"]?.checkpoint).fields.isEmpty)
}

@Test("TP-CHK-15 fields are untrusted: odd ones show read-only, and bad lists are dropped")
func editFieldsSanitized() throws {
    // Hidden characters in an editable value, a key too long to match, an unknown kind: read-only.
    let odd = try #require(withFields("[" + [
        field("to", "text", "me@example.test\u{202E}tset"),
        field(String(repeating: "k", count: 41), "text", "v"),
        field("mode", "slider", "3"),
        field("ok", "text", "fine"),
    ].joined(separator: ",") + "]"))
    #expect(odd.fields.map(\.kind) == [.fixed, .fixed, .fixed, .text])
    #expect(odd.fields[0].value == "me@example.testtset", "shown cleaned")
    // Labels are cleaned and capped.
    let label = try #require(withFields("[" + field("to", "text", "v", label: "To\u{202E}" + String(repeating: "x", count: 100)) + "]"))
    #expect(label.fields[0].label.count == CorralRunCard.Field.maxLabelLength)
    #expect(!label.fields[0].label.unicodeScalars.contains("\u{202E}"))
    // Duplicate keys, or more fields than the core ever sends: no fields at all.
    let twice = try #require(withFields("[" + field("to", "text", "a") + "," + field("to", "text", "b") + "]"))
    #expect(twice.fields.isEmpty)
    let many = (0...CorralRunCard.Field.maxFields).map { field("k\($0)", "text", "v") }.joined(separator: ",")
    #expect(try #require(withFields("[" + many + "]")).fields.isEmpty)
    let fits = (0..<CorralRunCard.Field.maxFields).map { field("k\($0)", "text", "v") }.joined(separator: ",")
    #expect(try #require(withFields("[" + fits + "]")).fields.count == CorralRunCard.Field.maxFields)
}

@Test("TP-CHK-16 Edit is offered only when a field can change")
func editOffered() throws {
    #expect(try approval(email).editable)
    #expect(try !approval("[" + field("urgent", "fixed", "true") + "]").editable)
    #expect(try !approval("[]").editable)
}

@Test("TP-CHK-16 the sheet sends only the editable fields the user changed")
func editChanges() throws {
    let a = try approval(email)
    let draft = ["to": "me@example.test", "body": "the history", "urgent": "false", "bcc": "x"]
    #expect(a.changes(draft) == ["to": "me@example.test"])
    #expect(a.changes([:]).isEmpty)
}

@Test("TP-CHK-16 Approve edited follows Approve's rules, needs a change, and numbers must be numbers")
func editHints() throws {
    let a = try approval(email)
    #expect(a.editHint(["to": "me@example.test"], pinned: false) == "Click the drop to pin this card, then approve.")
    #expect(a.editHint(["to": "me@example.test"], pinned: true) == nil)
    #expect(a.editHint([:], pinned: true) == "Change a field to approve the edited action.")
    for bad in ["", "three", "1e400", "nan", "0x10", "1,000", "inf"] {
        #expect(a.editHint(["retries": bad], pinned: true) == "Retries needs a number.", "\(bad)")
    }
    for good in ["1", " -2.5 ", "1e3", "0"] {
        #expect(a.editHint(["retries": good], pinned: true) == nil, "\(good)")
    }
    let critical = try approval(email, risk: "critical")
    #expect(critical.editHint(["to": "me@example.test"], pinned: true)
            == "A critical action can't be edited yet. Approve it with Touch ID, or deny.")
    let normal = try approval(email, risk: "normal")
    #expect(normal.editHint(["to": "me@example.test"], pinned: false) == nil)
}

@Test("TP-CHK-16 Approve edited is encoded exactly as the fixture Rust decodes")
func editIntentMatchesFixture() throws {
    let a = try approval(email)
    let button = try #require(a.approveEdited(["to": "me@example.test", "retries": "1", "urgent": "false"]))
    #expect(button == .approveEdited(checkpoint: "cp-1", changes: ["to": "me@example.test", "retries": "1"]))
    #expect(button.title == "Approve edited")
    let intent = CorralCardModel.intent(for: button, agentID: "scout")
    #expect(intent == .editCheckpoint(id: "scout", checkpoint: "cp-1", changes: ["to": "me@example.test", "retries": "1"]))
    let data = try #require(CorralIntentWire.encode(intent))
    #expect(try sameJSON(data, fixture("intent-edit-checkpoint-v1.json")))
    #expect(a.approveEdited([:]) == nil, "nothing changed, nothing to send")
}

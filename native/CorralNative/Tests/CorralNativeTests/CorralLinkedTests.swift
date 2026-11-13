// CorralLinkedTests.swift: TP-LINK-03..05, the Swift side of linked apps (ADR-005, docs/07 §8.3).
// Consumer Muse and your dot can't be driven by Corral, so their drops only open the app. Rust
// says which drops are linked and which linked apps are shown (Fixtures/bridge/
// roster-linked-v1.json); Swift draws the card and the right-click menu and reports clicks.
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

private func roster(_ agent: String, links: String? = nil) -> Result<CorralRoster, CorralRoster.DecodeError> {
    let tail = links.map { #","links":\#($0)"# } ?? ""
    return CorralRoster.decode(Data(#"{"v":1,"hidden":0,"agents":[\#(agent)]\#(tail)}"#.utf8))
}

@Test("TP-LINK-03 the roster marks linked drops and says which are shown, as Rust sends it")
func rosterCarriesLinkedApps() throws {
    let r = try CorralRoster.decode(fixture("roster-linked-v1.json")).get()
    #expect(r.agents.map(\.id) == ["grok", "muse", "dot"])
    #expect(r.agents.map(\.linkedApp) == [nil, "Muse", "ChatGPT"])
    #expect(r.links == CorralLinks(muse: true, dot: "felipe"))
}

@Test("TP-LINK-03 without links nothing is linked, and linked text is bounded")
func linkedTextBounded() throws {
    let plain = try CorralRoster.decode(fixture("roster-v1.json")).get()
    #expect(plain.links == CorralLinks(muse: false, dot: nil))
    #expect(plain.agents.allSatisfy { $0.linkedApp == nil })

    let long = String(repeating: "A", count: 100)
    let r = try roster(#"{"id":"a","name":"A","monogram":"A","status":"available","linked":"\#(long)"}"#).get()
    #expect(r.agents[0].linkedApp?.count == CorralRoster.maxLinkedAppLength)

    let control = try roster(#"{"id":"a","name":"A","monogram":"A","status":"available","linked":"Mu\u0000se‮"}"#).get()
    #expect(control.agents[0].linkedApp == "Muse")

    let empty = try roster(#"{"id":"a","name":"A","monogram":"A","status":"available","linked":" \u0000 "}"#).get()
    #expect(empty.agents[0].linkedApp == nil, "an empty app name isn't a linked drop")

    let dot = try roster(#"{"id":"a","name":"A","monogram":"A","status":"available"}"#,
                         links: #"{"muse":false,"dot":"fel\u0000ipe"}"#).get()
    #expect(dot.links.dot == "felipe")
}

@Test("TP-LINK-03 unknown keys in links reject the roster, like everywhere on the wire")
func linksAreStrict() {
    let r = roster(#"{"id":"a","name":"A","monogram":"A","status":"available"}"#,
                   links: #"{"muse":true,"url":"https://evil.example"}"#)
    #expect(r == .failure(.malformed))
}

@Test("TP-LINK-05 a linked drop's card opens the app and has no task field")
func linkedCard() {
    let m = CorralCardModel(agentName: "Muse", card: nil, linkedApp: "Muse")
    #expect(m.label == "MUSE · LINKED APP")
    #expect(m.headline == "Works in Muse")
    #expect(m.status == "Not managed by Corral")
    #expect(m.buttons == [.open("Muse")])
    #expect(m.buttons[0].title == "Open Muse")
    #expect(m.buttons[0].isPrimary)
    #expect(m.fieldPlaceholder == nil, "Corral can't give a linked app a task")
    #expect(CorralCardModel.intent(for: .open("Muse"), agentID: "muse") == .openLinked(id: "muse"))

    let dot = CorralCardModel(agentName: "Your dot", card: nil, linkedApp: "ChatGPT")
    #expect(dot.headline == "Works in ChatGPT")
    #expect(dot.buttons == [.open("ChatGPT")])
}

@Test("TP-LINK-04 menu and card intents encode as the fixtures Rust decodes")
func linkedIntentsMatchFixtures() throws {
    let cases: [(CorralIntent, String)] = [
        (.showLinked(id: "dot", look: "felipe"), "intent-show-linked-v1.json"),
        (.hideLinked(id: "muse"), "intent-hide-linked-v1.json"),
        (.openLinked(id: "muse"), "intent-open-linked-v1.json"),
    ]
    for (intent, name) in cases {
        let sent = try #require(CorralIntentWire.encode(intent))
        #expect(try sameJSON(sent, fixture(name)), "\(name)")
    }
    let showMuse = try #require(CorralIntentWire.encode(.showLinked(id: "muse", look: nil)))
    let object = try #require(try JSONSerialization.jsonObject(with: showMuse) as? [String: Any])
    let body = try #require(object["intent"] as? [String: Any])
    #expect(body["look"] == nil, "no look key for Muse")
}

@Test("TP-LINK-04 the right-click menu shows what's linked and asks Rust to change it")
func linkedMenu() {
    let off = CorralLinkedMenuModel(links: CorralLinks(muse: false, dot: nil))
    #expect(off.muse == .init(title: "Muse", checked: false, intent: .showLinked(id: "muse", look: nil)))
    #expect(off.dotLooks.map(\.title) == ["Ring", "Felipe", "Alfred", "Iggy", "Todd"])
    #expect(off.dotLooks.allSatisfy { !$0.checked })
    #expect(off.dotLooks[1].intent == .showLinked(id: "dot", look: "felipe"))
    #expect(off.hideDot == nil)

    let on = CorralLinkedMenuModel(links: CorralLinks(muse: true, dot: "iggy"))
    #expect(on.muse == .init(title: "Muse", checked: true, intent: .hideLinked(id: "muse")))
    #expect(on.dotLooks.filter(\.checked).map(\.title) == ["Iggy"])
    #expect(on.hideDot == .init(title: "Hide your dot", checked: false, intent: .hideLinked(id: "dot")))
}

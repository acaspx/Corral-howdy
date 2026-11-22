// CorralPromptBarLogoTests.swift: each @ suggestion shows its agent's logo (docs/07 §6.1,
// ADR-015, TP-BAR-10). The path is checked like a drop's: a path that isn't a logo in a logos
// folder is dropped, and the row shows the name alone.
import Testing
import Foundation
@testable import CorralNative

private func fixture(_ name: String) throws -> Data {
    let root = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    return try Data(contentsOf: root.appendingPathComponent("Fixtures/bridge/\(name)"))
}

private func row(logo: String) -> String {
    #"{"v":1,"seq":1,"view":{"chips":[],"placeholder":"p","button":{"label":"Deploy all (1)","enabled":true,"all":true},"error":null,"suggestions":[{"id":"claude","name":"Claude","shown":true,"status":"ready","completion":"@Claude ","logo":\#(logo)}]}}"#
}

@Test("TP-BAR-10 a suggestion's logo decodes from the fixture Rust sends")
func suggestionLogoDecodes() throws {
    let answer = try CorralPromptBarSnapshot.decode(fixture("prompt-bar-logos-v1.json")).get()
    #expect(answer.view.suggestions.first?.logoPath
        == "/Users/you/Library/Application Support/Corral/logos/anthropic.png")
    let plain = try CorralPromptBarSnapshot.decode(fixture("prompt-bar-v1.json")).get()
    #expect(plain.view.suggestions.first?.logoPath == nil, "no logo key, no logo")
}

@Test("TP-BAR-10 a logo path that isn't a logo in a logos folder is dropped; the row stays")
func badLogoPathsAreDropped() throws {
    for bad in [#""/etc/passwd""#, #""/Users/you/logos/../secrets.png""#, #""logos/anthropic.png""#] {
        let answer = try CorralPromptBarSnapshot.decode(Data(row(logo: bad).utf8)).get()
        #expect(answer.view.suggestions.count == 1, "\(bad)")
        #expect(answer.view.suggestions.first?.logoPath == nil, "\(bad)")
    }
    guard case .failure = CorralPromptBarSnapshot.decode(Data(row(logo: "7").utf8)) else {
        Issue.record("a logo that isn't a string was accepted")
        return
    }
}

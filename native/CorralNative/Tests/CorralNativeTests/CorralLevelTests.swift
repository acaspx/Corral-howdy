// CorralLevelTests.swift: where the corral sits (docs/07 §4, "Level setting").
// The corral sits on the desktop unless the user picks otherwise, and the choice is remembered.
import Testing
import Foundation
@testable import CorralNative

private func scratchDefaults() -> UserDefaults {
    let name = "corral.tests.\(UUID().uuidString)"
    let defaults = UserDefaults(suiteName: name)!
    defaults.removePersistentDomain(forName: name)
    return defaults
}

@Test("The corral sits on the desktop until the user picks otherwise")
func levelDefaultsToDesktop() {
    #expect(CorralLevel.load(from: scratchDefaults()) == .desktop)
}

@Test("The user's choice is remembered")
func levelIsRemembered() {
    let defaults = scratchDefaults()
    for level in CorralLevel.allCases {
        level.save(to: defaults)
        #expect(CorralLevel.load(from: defaults) == level)
    }
}

@Test("A stored value this build doesn't know falls back to the desktop")
func unknownLevelFallsBack() {
    let defaults = scratchDefaults()
    defaults.set("sideways", forKey: CorralLevel.defaultsKey)
    #expect(CorralLevel.load(from: defaults) == .desktop)
}

@Test("The menu offers three places, in order, in plain words")
func levelMenuTitles() {
    #expect(CorralLevel.allCases == [.desktop, .normal, .floating])
    #expect(CorralLevel.allCases.map(\.title) == ["On the desktop", "Like other windows", "Always on top"])
}

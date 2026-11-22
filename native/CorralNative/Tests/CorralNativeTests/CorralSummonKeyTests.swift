// CorralSummonKeyTests.swift: the prompt bar's summon shortcut (docs/07 §6.1, TP-BAR-09).
import Testing
import Foundation
import Carbon.HIToolbox
@testable import CorralNative

private func scratchDefaults() -> UserDefaults {
    let name = "corral.tests.summon.\(UUID().uuidString)"
    let defaults = UserDefaults(suiteName: name)!
    defaults.removePersistentDomain(forName: name)
    return defaults
}

@Test("TP-BAR-09 ⌃⌥Space by default; a choice is remembered; an unknown saved value falls back")
func summonKeyIsRemembered() {
    let defaults = scratchDefaults()
    #expect(CorralSummonKey.load(from: defaults) == .controlOptionSpace)
    CorralSummonKey.off.save(to: defaults)
    #expect(CorralSummonKey.load(from: defaults) == .off)
    defaults.set("hyper-space", forKey: CorralSummonKey.defaultsKey)
    #expect(CorralSummonKey.load(from: defaults) == .controlOptionSpace)
}

@Test("TP-BAR-09 each choice names its keys, and Off registers nothing")
func summonKeyChoices() {
    #expect(CorralSummonKey.allCases.map(\.title) == ["⌃⌥Space", "⌃⌥P", "⌃⇧Space", "Off"])
    let space = CorralSummonKey.controlOptionSpace.carbon
    #expect(space?.keyCode == UInt32(kVK_Space))
    #expect(space?.modifiers == UInt32(controlKey | optionKey))
    #expect(CorralSummonKey.controlOptionP.carbon?.keyCode == UInt32(kVK_ANSI_P))
    #expect(CorralSummonKey.controlShiftSpace.carbon?.modifiers == UInt32(controlKey | shiftKey))
    #expect(CorralSummonKey.off.carbon == nil)
}

@Test("TP-BAR-09 the shortcut opens the bar, and again closes it, keeping the draft")
func summonToggles() {
    var bar = CorralPromptBarState()
    #expect(bar.summoned() == .edited)
    #expect(bar.isOpen)
    _ = bar.edit(text: "plan", cursor: 4)
    #expect(bar.summoned() == .close)
    #expect(!bar.isOpen && bar.text == "plan")
}

@Test("TP-BAR-09 on the desktop level, the corral comes up while the bar is open")
func corralComesUpWithTheBar() {
    #expect(CorralLevel.desktop.whileBarOpen == .floating)
    #expect(CorralLevel.normal.whileBarOpen == .floating)
    #expect(CorralLevel.floating.whileBarOpen == .floating)
}

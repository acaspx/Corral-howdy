// CorralRobustnessTests.swift: TP-SHAPE-08..10 and TP-PLACE-04 (docs/04-test-plan.md)
// The corral runs in the same process as the Rust core, so bad input must never crash it.
import Testing
import AppKit
import SwiftUI
import CoreGraphics
@testable import CorralNative

@Test("TP-SHAPE-08 duplicate agent IDs are shown once, keeping the first (highest-ranked) one")
func duplicateIDsAreDropped() {
    let (blobs, _, _, shape) = CorralLayout.layout(rankedIDs: ["a", "b", "a", "c", "b"])
    #expect(blobs.map(\.id) == ["a", "b", "c"])
    #expect(blobs.map(\.rank) == [1, 2, 3])
    #expect(shape == .polygon(sides: 3))
}

@MainActor
@Test("TP-SHAPE-09 the morph never traps on duplicate IDs")
func morphToleratesDuplicates() {
    let morph = CorralMorph()
    let blobs = CorralLayout.layout(rankedIDs: ["a", "b", "c"]).blobs
    morph.setTarget(blobs + blobs, reduceMotion: true)
    #expect(morph.blobs.map(\.id) == ["a", "b", "c"])
    morph.setTarget(blobs + [blobs[0]], reduceMotion: false)
    #expect(Set(morph.blobs.map(\.id)) == ["a", "b", "c"])
}

/// Steps the morph at `hz` frames per second until it settles. Returns the simulated seconds.
@MainActor
private func settleTime(hz: Double) -> Double {
    let morph = CorralMorph()
    morph.setTarget(CorralLayout.layout(rankedIDs: ["a", "b", "c"]).blobs, reduceMotion: true)
    morph.setTarget(CorralLayout.layout(rankedIDs: ["c", "a", "b", "d"]).blobs, reduceMotion: false)
    var now = Date(timeIntervalSinceReferenceDate: 0)
    morph.advance(to: now)
    var elapsed = 0.0
    while morph.isSettling && elapsed < 5 {
        now += 1 / hz
        elapsed += 1 / hz
        morph.advance(to: now)
    }
    return elapsed
}

@MainActor
@Test("TP-SHAPE-10 morph timing follows real time, not frame rate (60 Hz vs 120 Hz ProMotion)")
func morphIsFrameRateIndependent() {
    let at60 = settleTime(hz: 60), at120 = settleTime(hz: 120)
    #expect(at60 < 0.8 + 0.5)                    // settles (TP-SHAPE-07 budget plus fade-in)
    #expect(abs(at60 - at120) / at60 < 0.2)      // same feel on every display (old fixed-step code: 2x)
}

@MainActor
@Test("TP-SHAPE-11 a long stall is clamped, so the spring never explodes")
func morphClampsStalls() {
    let morph = CorralMorph()
    morph.setTarget(CorralLayout.layout(rankedIDs: ["a", "b"]).blobs, reduceMotion: true)
    morph.setTarget(CorralLayout.layout(rankedIDs: ["b", "a", "c"]).blobs, reduceMotion: false)
    let start = Date(timeIntervalSinceReferenceDate: 0)
    morph.advance(to: start)
    morph.advance(to: start + 3)                 // app was asleep for 3 s
    for blob in morph.blobs {
        #expect(hypot(blob.offset.x, blob.offset.y) < 400)
    }
}

@MainActor
@Test("TP-PLACE-04 the corral panel can take keyboard focus (prompt bar, hover card keys)")
func panelCanBecomeKey() {
    let panel = CorralPanel(rootView: EmptyView())
    #expect(panel.canBecomeKey)
    #expect(!panel.canBecomeMain)
}

@MainActor
@Test("TP-PLACE-05 default placement uses the menu-bar display, not the key window's display")
func defaultScreenIsMenuBarScreen() {
    #expect(CorralPanel.defaultScreen() === NSScreen.screens.first)
}

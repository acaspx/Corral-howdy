// CorralHostingView.swift
// Corral's panels never activate the app (docs/07 §5), so AppKit can treat a click on a drop or a
// card button as only bringing the window forward, and the button never fires. Accepting the first
// mouse makes one click act, whatever app is in front.

import AppKit
import SwiftUI

final class CorralHostingView<Content: View>: NSHostingView<Content> {
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }
}

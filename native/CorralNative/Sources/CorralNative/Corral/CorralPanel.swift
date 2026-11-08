// CorralPanel.swift
// The borderless, non-activating window that holds the corral at the bottom centre
// of the screen, just above the Dock. Status: SPIKE S5 prototype. Builds in CI.

import AppKit
import SwiftUI

public enum CorralLevel: String, Sendable {
    case floating   // above app windows (default): always reachable for checkpoints
    case desktop    // sits on the desktop layer, behind app windows, like a desktop object
}

@MainActor
public final class CorralPanel: NSPanel {

    public init<Content: View>(rootView: Content, level corralLevel: CorralLevel = .floating) {
        super.init(contentRect: .zero,
                   styleMask: [.borderless, .nonactivatingPanel],
                   backing: .buffered, defer: false)
        isOpaque = false
        backgroundColor = .clear
        hasShadow = false                 // the shader draws its own contact shadow
        isMovableByWindowBackground = false
        hidesOnDeactivate = false
        becomesKeyOnlyIfNeeded = true     // clicking a token doesn't steal focus from the user's app
        collectionBehavior = [.canJoinAllSpaces, .stationary, .ignoresCycle]
        apply(level: corralLevel)
        contentView = NSHostingView(rootView: rootView)
    }

    /// Borderless windows can't become key by default. The corral needs keyboard focus for the
    /// prompt bar and the hover card's keyboard path (TP-PLACE-04, TP-HOVER-07). With
    /// `becomesKeyOnlyIfNeeded`, plain clicks on drops still leave the user's app focused.
    public override var canBecomeKey: Bool { true }
    public override var canBecomeMain: Bool { false }

    /// The display with the menu bar (docs/07 §5), not `NSScreen.main`, which is whichever
    /// display holds the key window (TP-PLACE-05).
    public static func defaultScreen() -> NSScreen? { NSScreen.screens.first }

    public func apply(level: CorralLevel) {
        switch level {
        case .floating:
            self.level = .floating
        case .desktop:
            self.level = NSWindow.Level(rawValue: Int(CGWindowLevelForKey(.desktopIconWindow)) + 1)
        }
    }

    /// Bottom-centre of the screen's visible frame, so the corral never overlaps the Dock
    /// or the menu bar wherever the user keeps them.
    public func place(canvas: CGSize, on screen: NSScreen? = CorralPanel.defaultScreen(), bottomMargin: CGFloat = 12) {
        guard let screen else { return }
        let vf = screen.visibleFrame
        let origin = CGPoint(x: vf.midX - canvas.width / 2, y: vf.minY + bottomMargin)
        setFrame(CGRect(origin: origin, size: canvas), display: true, animate: false)
    }
}

/// FALLBACK ONLY (plan C in docs/07 §4): the wallpaper behind the corral's frame, used as the
/// glass backdrop if Spike S5 finds the Metal pass can't sample the system glass layer.
/// Needs no Screen Recording permission. Approximation: assumes "Fill Screen" scaling.
@MainActor
public enum CorralBackdrop {
    public static func wallpaperCrop(for frame: CGRect, on screen: NSScreen) -> NSImage? {
        guard let url = NSWorkspace.shared.desktopImageURL(for: screen),
              let image = NSImage(contentsOf: url),
              let cg = image.cgImage(forProposedRect: nil, context: nil, hints: nil) else { return nil }

        let screenSize = screen.frame.size
        let imgSize = CGSize(width: cg.width, height: cg.height)
        let scale = max(screenSize.width / imgSize.width, screenSize.height / imgSize.height)
        let drawn = CGSize(width: imgSize.width * scale, height: imgSize.height * scale)
        let inset = CGPoint(x: (drawn.width - screenSize.width) / 2, y: (drawn.height - screenSize.height) / 2)

        // Screen coords (origin bottom-left) → image pixel coords (origin top-left).
        let local = CGPoint(x: frame.minX - screen.frame.minX, y: frame.minY - screen.frame.minY)
        let px = (local.x + inset.x) / scale
        let py = (drawn.height - (local.y + inset.y) - frame.height) / scale
        let rect = CGRect(x: px, y: py, width: frame.width / scale, height: frame.height / scale)
        guard let crop = cg.cropping(to: rect.integral) else { return nil }
        return NSImage(cgImage: crop, size: frame.size)
    }
}

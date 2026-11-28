// The UI test proof's app (Spike S2, step 4): a window with a button, and a floating,
// non-activating panel with another, as the corral is. Each button counts its clicks in a label
// the test reads.
import AppKit
import SwiftUI

@main
struct ProofApp: App {
    @NSApplicationDelegateAdaptor(ProofDelegate.self) private var delegate

    var body: some Scene {
        WindowGroup("Corral UI proof") {
            Counter(button: "Click in the window", id: "window")
                .frame(minWidth: 320, minHeight: 160)
        }
    }
}

/// A button and how many times it was clicked.
struct Counter: View {
    let button: String
    let id: String
    @State private var clicks = 0

    var body: some View {
        VStack(spacing: 12) {
            Text(clicks == 0 ? "Not clicked" : "Clicked \(clicks)")
                .accessibilityIdentifier("\(id)-status")
            Button(button) { clicks += 1 }
                .accessibilityIdentifier("\(id)-button")
        }
        .padding(24)
    }
}

/// Shows the panel at launch, like the corral: it floats, and clicking it doesn't make the app
/// active.
@MainActor
final class ProofDelegate: NSObject, NSApplicationDelegate {
    private var panel: NSPanel?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let panel = NSPanel(
            contentRect: NSRect(x: 120, y: 120, width: 320, height: 160),
            styleMask: [.titled, .nonactivatingPanel, .utilityWindow],
            backing: .buffered,
            defer: false
        )
        panel.title = "Corral UI proof panel"
        panel.isFloatingPanel = true
        panel.level = .floating
        panel.hidesOnDeactivate = false
        panel.contentView = NSHostingView(rootView: Counter(button: "Click in the panel", id: "panel"))
        panel.orderFrontRegardless()
        self.panel = panel
    }
}

// CorralPlayground: try the liquid-glass corral on your own desktop, with fake agents.
//
// There is no Rust core here. A small control window plays the core's part: it decides which
// agents exist, their order (most used first) and their status, and hands that to the real
// `CorralPanel` + `CorralGlassView`, exactly as the core will over the C ABI. Spike S5/S6 tool.
//
// Build and run (macOS 26, Xcode 26), from native/CorralNative:
//   xcodebuild -scheme CorralPlayground -destination 'platform=macOS' -derivedDataPath .build/xcode build
//   .build/xcode/Build/Products/Debug/CorralPlayground --agents 5
// Use xcodebuild, not `swift run`: SwiftPM's command line doesn't compile the Metal shader.

import AppKit
import CorralNative
import SwiftUI

@main
@MainActor
enum CorralPlaygroundApp {
    static func main() {
        let app = NSApplication.shared
        let delegate = PlaygroundDelegate(agentCount: initialAgentCount())
        app.delegate = delegate
        app.setActivationPolicy(.regular)
        app.run()   // blocks; keeps `delegate` alive (NSApplication holds it weakly)
    }

    /// `--agents N` (0…12, default 5).
    static func initialAgentCount() -> Int {
        let args = CommandLine.arguments
        guard let i = args.firstIndex(of: "--agents"), i + 1 < args.count, let n = Int(args[i + 1]) else { return 5 }
        return min(max(n, 0), 12)
    }
}

/// Plays the Rust core's part: the agents, their usage order and their status.
@MainActor
final class PlaygroundModel: ObservableObject {
    private static let names: [(String, String)] = [
        ("Scout", "Sc"), ("Claude Code", "CC"), ("Muse", "Mu"), ("Ledger", "Le"), ("Codex", "Cx"),
        ("Atlas", "At"), ("Pixel", "Px"), ("Echo", "Ec"), ("Nova", "Nv"), ("Weaver", "We"),
    ]

    /// Every agent, most used first. The corral shows the first eight.
    @Published var agents: [CorralAgentToken] = []
    @Published var log: [String] = []
    @Published var level: CorralLevel = .floating
    private var created = 0

    init(agentCount: Int) {
        for _ in 0..<agentCount { add() }
    }

    var shown: [CorralAgentToken] { Array(agents.prefix(CorralMetrics.maxShown)) }
    var hiddenCount: Int { max(0, agents.count - CorralMetrics.maxShown) }

    func add() {
        let (name, monogram) = Self.names[created % Self.names.count]
        let round = created / Self.names.count
        created += 1
        agents.append(CorralAgentToken(id: "agent-\(created)",
                                       name: round == 0 ? name : "\(name) \(round + 1)",
                                       monogram: monogram))
    }

    func removeLast() {
        if !agents.isEmpty { agents.removeLast() }
    }

    /// The least-used shown agent becomes the most used: it should glide around the ring to top-left.
    func promoteLeastUsed() {
        guard shown.count > 1 else { return }
        let agent = agents.remove(at: shown.count - 1)
        agents.insert(agent, at: 0)
    }

    func shuffle() { agents.shuffle() }

    func record(_ intent: CorralIntent) {
        switch intent {
        case .openAgent(let id):
            note("openAgent \(id)")
            cycleStatus(of: id)
        case .deploy(let ids):
            note("deploy \(ids.joined(separator: ", "))")
        case .summonPromptBar:
            note("summonPromptBar")
        }
    }

    private func cycleStatus(of id: String) {
        guard let i = agents.firstIndex(where: { $0.id == id }) else { return }
        agents[i].status = switch agents[i].status {
        case .available: .working
        case .working: .needsYou
        case .needsYou: .done
        case .done: .error
        case .error: .available
        }
        note("  \(agents[i].name) is now \(agents[i].status)")
    }

    private func note(_ line: String) {
        log.insert(line, at: 0)
        if log.count > 10 { log.removeLast(log.count - 10) }
    }
}

/// What the corral panel shows. Reports shown-ID changes so the panel can resize to the new shape.
struct CorralRoot: View {
    @ObservedObject var model: PlaygroundModel
    let onShownChange: @MainActor ([String]) -> Void

    var body: some View {
        CorralGlassView(agents: model.shown, hiddenCount: model.hiddenCount) { intent in
            model.record(intent)
        }
        .onChange(of: model.shown.map(\.id)) { _, ids in onShownChange(ids) }
    }
}

struct PlaygroundControls: View {
    @ObservedObject var model: PlaygroundModel
    let onLevelChange: @MainActor (CorralLevel) -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("\(model.agents.count) agents, \(model.shown.count) shown").font(.headline)
            HStack {
                Button("Add agent") { model.add() }
                Button("Remove") { model.removeLast() }.disabled(model.agents.isEmpty)
            }
            HStack {
                Button("Promote least used") { model.promoteLeastUsed() }.disabled(model.shown.count < 2)
                Button("Shuffle") { model.shuffle() }.disabled(model.agents.count < 2)
            }
            Picker("Corral level", selection: $model.level) {
                Text("Float above windows").tag(CorralLevel.floating)
                Text("Desktop only").tag(CorralLevel.desktop)
            }
            .pickerStyle(.radioGroup)
            .onChange(of: model.level) { _, level in onLevelChange(level) }

            Text("Click a drop to cycle its status (available, working, needs you, done, error). Command-click selects. Try Reduce Motion and Reduce Transparency in System Settings › Accessibility › Display.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            Divider()
            Text("Intents sent to the core").font(.caption.bold())
            ForEach(Array(model.log.enumerated()), id: \.offset) { _, line in
                Text(line).font(.caption.monospaced())
            }
            Spacer(minLength: 0)
        }
        .padding(16)
        .frame(width: 340, alignment: .leading)
        .frame(minHeight: 420, alignment: .top)
    }
}

@MainActor
final class PlaygroundDelegate: NSObject, NSApplicationDelegate {
    private let model: PlaygroundModel
    private var panel: CorralPanel?
    private var controls: NSWindow?
    private var observers: [NSObjectProtocol] = []   // kept so the observers live as long as the app

    init(agentCount: Int) {
        model = PlaygroundModel(agentCount: agentCount)
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        installMenu()

        let panel = CorralPanel(rootView: CorralRoot(model: model) { [weak self] ids in
            self?.resizePanel(for: ids)
        })
        self.panel = panel
        resizePanel(for: model.shown.map(\.id))
        panel.orderFrontRegardless()

        let controls = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 340, height: 420),
                                styleMask: [.titled, .closable, .miniaturizable],
                                backing: .buffered, defer: false)
        controls.title = "Corral Playground"
        controls.isReleasedWhenClosed = false
        controls.contentView = NSHostingView(rootView: PlaygroundControls(model: model) { [weak self] level in
            self?.panel?.apply(level: level)
        })
        controls.center()
        controls.makeKeyAndOrderFront(nil)
        self.controls = controls

        // Closing the control window quits the playground (the corral panel has no close button).
        observers.append(NotificationCenter.default.addObserver(forName: NSWindow.willCloseNotification,
                                                                object: controls, queue: .main) { _ in
            MainActor.assumeIsolated { NSApp.terminate(nil) }
        })
        // Displays plugged in, unplugged or rearranged: put the corral back at the bottom centre.
        observers.append(NotificationCenter.default.addObserver(
            forName: NSApplication.didChangeScreenParametersNotification, object: nil, queue: .main) { [weak self] _ in
            MainActor.assumeIsolated { self?.resizePanel(for: self?.model.shown.map(\.id) ?? []) }
        })
        NSApp.activate()
    }

    /// The canvas grows and shrinks with the shape; keep the panel at the bottom centre.
    private func resizePanel(for ids: [String]) {
        panel?.place(canvas: CorralLayout.layout(rankedIDs: ids).canvas)
    }

    /// A minimal app menu so Command-Q works.
    private func installMenu() {
        let main = NSMenu()
        let appItem = NSMenuItem()
        main.addItem(appItem)
        let appMenu = NSMenu()
        appMenu.addItem(withTitle: "Quit Corral Playground",
                        action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        appItem.submenu = appMenu
        NSApp.mainMenu = main
    }
}

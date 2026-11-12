// CorralPlayground: try the liquid-glass corral and its cards on your own desktop, with fake agents.
//
// There is no Rust core here. A small control window plays the core's part: it decides which
// agents exist, their order (most used first), their runs and status, and hands that to the real
// `CorralPanel`, `CorralGlassView` and card controller, exactly as the core will over the C ABI.
// Spike S5/S6 tool.
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

/// Plays the Rust core's part: the agents, their usage order, their runs and status.
@MainActor
final class PlaygroundModel: ObservableObject {
    private static let names: [(String, String)] = [
        ("Scout", "Sc"), ("Claude Code", "CC"), ("Muse", "Mu"), ("Ledger", "Le"), ("Codex", "Cx"),
        ("Atlas", "At"), ("Pixel", "Px"), ("Echo", "Ec"), ("Nova", "Nv"), ("Weaver", "We"),
    ]

    /// A made-up run: works for 4 seconds, then answers (or fails if the task says "fail").
    struct FakeRun {
        var prompt: String
        var started: Date
        var ended: Date?
        var status: CorralRunCard.Status
        var answer: String?
        var error: String?
    }

    /// Every agent, most used first. The corral shows the first eight.
    @Published var agents: [CorralAgentToken] = []
    @Published var log: [String] = []
    @Published var level: CorralLevel = .floating
    @Published var hoverSource: CorralHoverSource = .swiftUI
    private var runs: [String: FakeRun] = [:]
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
        if let last = agents.popLast() { runs[last.id] = nil }
    }

    /// The least-used shown agent becomes the most used: it should glide around the ring to top-left.
    func promoteLeastUsed() {
        guard shown.count > 1 else { return }
        let agent = agents.remove(at: shown.count - 1)
        agents.insert(agent, at: 0)
    }

    func shuffle() { agents.shuffle() }

    /// What the core would do with each intent from the corral or a card.
    func record(_ intent: CorralIntent) {
        switch intent {
        case .openAgent(let id):
            note("openAgent \(name(of: id))")
        case .deploy(let ids):
            note("deploy \(ids.joined(separator: ", "))")
        case .summonPromptBar:
            note("summonPromptBar")
        case .deployTask(let id, let prompt):
            note("deployTask \(name(of: id)): \(prompt)")
            start(id, prompt: prompt)
        case .stop(let id):
            note("stop \(name(of: id))")
            finish(id, as: .stopped)
        case .sendBack(let id):
            note("sendBack \(name(of: id))")
            runs[id] = nil
            setStatus(id, .available)
        case .retry(let id):
            note("retry \(name(of: id))")
            if let prompt = runs[id]?.prompt { start(id, prompt: prompt) }
        }
    }

    /// The run cards the core would send for the shown drops at `now`.
    func cards(at now: Date) -> [String: CorralRunCard] {
        var cards: [String: CorralRunCard] = [:]
        for agent in shown {
            guard let run = runs[agent.id] else { continue }
            let ms = Int((run.ended ?? now).timeIntervalSince(run.started) * 1000)
            let micro = (ms / 1000) * 37 + (run.answer == nil ? 0 : 306)
            cards[agent.id] = CorralRunCard(status: run.status, prompt: run.prompt, elapsedMs: ms,
                                            spend: Self.usd(micro), answer: run.answer,
                                            error: run.error, actions: Self.actions(run.status))
        }
        return cards
    }

    func note(_ line: String) {
        log.insert(line, at: 0)
        if log.count > 16 { log.removeLast(log.count - 16) }
    }

    private func start(_ id: String, prompt: String) {
        let started = Date()
        runs[id] = FakeRun(prompt: prompt, started: started, ended: nil, status: .working)
        setStatus(id, .working)
        DispatchQueue.main.asyncAfter(deadline: .now() + 4) { [weak self] in
            MainActor.assumeIsolated {
                guard let self, self.runs[id]?.started == started, self.runs[id]?.status == .working else { return }
                if prompt.localizedCaseInsensitiveContains("fail") {
                    self.runs[id]?.error = "Couldn't reach the provider. (A made-up error: your task said \u{201C}fail\u{201D}.)"
                    self.finish(id, as: .error)
                } else {
                    self.runs[id]?.answer = Self.answer(to: prompt)
                    self.finish(id, as: .done)
                }
            }
        }
    }

    private func finish(_ id: String, as status: CorralRunCard.Status) {
        guard runs[id] != nil, runs[id]?.ended == nil else { return }
        runs[id]?.ended = Date()
        runs[id]?.status = status
        let token: CorralAgentToken.Status = switch status {
        case .done: .done
        case .error: .error
        case .needsYou: .needsYou
        case .queued, .working: .working
        case .stopped: .available
        }
        setStatus(id, token)
    }

    private func setStatus(_ id: String, _ status: CorralAgentToken.Status) {
        guard let i = agents.firstIndex(where: { $0.id == id }) else { return }
        agents[i].status = status
    }

    private func name(of id: String) -> String {
        agents.first(where: { $0.id == id })?.name ?? id
    }

    static func actions(_ status: CorralRunCard.Status) -> [CorralRunCard.Action] {
        switch status {
        case .queued, .working: [.stop]
        case .needsYou: []
        case .done, .stopped: [.sendBack, .newTask]
        case .error: [.retry, .sendBack]
        }
    }

    static func usd(_ micro: Int) -> String {
        micro % 10_000 == 0 ? String(format: "$%.2f", Double(micro) / 1e6)
                            : String(format: "$%.4f", Double(micro) / 1e6)
    }

    /// A made-up answer. Put "long" in the task to get one that scrolls.
    static func answer(to prompt: String) -> String {
        let one = "This is a made-up answer to \u{201C}\(prompt)\u{201D}. In the real app, the agent's reply appears here, cleaned and capped by the core."
        guard prompt.localizedCaseInsensitiveContains("long") else { return one }
        let more = (1...12).map { "Paragraph \($0): a long answer scrolls inside this box, so the card stays within its height budget above the corral." }
        return ([one] + more).joined(separator: "\n\n")
    }
}

/// Lets the corral's closures reach the card controller, which is created after the panel.
@MainActor
final class CardRelay {
    var controller: CorralCardController?
}

/// What the corral panel shows. Reports shown-ID changes so the panel can resize to the new shape.
struct CorralRoot: View {
    @ObservedObject var model: PlaygroundModel
    let relay: CardRelay
    let onShownChange: @MainActor ([String]) -> Void

    var body: some View {
        CorralGlassView(agents: model.shown, hiddenCount: model.hiddenCount, onIntent: { intent in
            MainActor.assumeIsolated {
                model.record(intent)
                if case .openAgent(let id) = intent { relay.controller?.dropClicked(id) }
            }
        }, onHoverDrop: { id, inside in
            MainActor.assumeIsolated { relay.controller?.swiftUIHover(drop: id, inside: inside) }
        })
        .onChange(of: model.shown.map(\.id)) { _, ids in onShownChange(ids) }
    }
}

struct PlaygroundControls: View {
    @ObservedObject var model: PlaygroundModel
    let onLevelChange: @MainActor (CorralLevel) -> Void
    let onHoverSourceChange: @MainActor (CorralHoverSource) -> Void

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

            Picker("Hover detection", selection: $model.hoverSource) {
                ForEach(CorralHoverSource.allCases, id: \.self) { source in
                    Text(source.rawValue).tag(source)
                }
            }
            .pickerStyle(.radioGroup)
            .onChange(of: model.hoverSource) { _, source in onHoverSourceChange(source) }

            Text("Spike S6: rest the pointer on a drop while another app is in front, then click a drop and type a task (Return sends; \u{201C}fail\u{201D} fails it, \u{201C}long\u{201D} gives a long answer). Close a pinned card with ×, Esc, or a click anywhere else.")
                .font(.caption)
                .foregroundStyle(.secondary)
                .fixedSize(horizontal: false, vertical: true)

            Divider()
            Text("What happened (newest first)").font(.caption.bold())
            ForEach(Array(model.log.enumerated()), id: \.offset) { _, line in
                Text(line).font(.caption.monospaced()).lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .padding(16)
        .frame(width: 360, alignment: .leading)
        .frame(minHeight: 560, alignment: .top)
    }
}

@MainActor
final class PlaygroundDelegate: NSObject, NSApplicationDelegate {
    private let model: PlaygroundModel
    private let relay = CardRelay()
    private var panel: CorralPanel?
    private var controls: NSWindow?
    private var ticker: Timer?
    private var observers: [NSObjectProtocol] = []   // kept so the observers live as long as the app

    init(agentCount: Int) {
        model = PlaygroundModel(agentCount: agentCount)
        super.init()
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        installMenu()

        let panel = CorralPanel(rootView: CorralRoot(model: model, relay: relay) { [weak self] ids in
            self?.resizePanel(for: ids)
        })
        self.panel = panel
        resizePanel(for: model.shown.map(\.id))
        panel.orderFrontRegardless()

        let controller = CorralCardController(corralPanel: panel) { [weak self] intent in
            MainActor.assumeIsolated {
                self?.model.record(intent)
                self?.refreshCards()
            }
        }
        controller.onLog = { [weak self] line in
            MainActor.assumeIsolated { self?.model.note(line) }
        }
        relay.controller = controller
        refreshCards()

        // The core sends cards on every change and once a second while a clock runs.
        ticker = Timer.scheduledTimer(withTimeInterval: 1, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.refreshCards() }
        }

        let controls = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 360, height: 560),
                                styleMask: [.titled, .closable, .miniaturizable],
                                backing: .buffered, defer: false)
        controls.title = "Corral Playground"
        controls.isReleasedWhenClosed = false
        controls.contentView = NSHostingView(rootView: PlaygroundControls(
            model: model,
            onLevelChange: { [weak self] level in self?.panel?.apply(level: level) },
            onHoverSourceChange: { [weak self] source in self?.relay.controller?.hoverSource = source }
        ))
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

    func applicationWillTerminate(_ notification: Notification) {
        ticker?.invalidate()
        relay.controller?.stop()
    }

    private func refreshCards() {
        relay.controller?.update(agents: model.shown, cards: model.cards(at: Date()))
    }

    /// The canvas grows and shrinks with the shape; keep the panel at the bottom centre.
    private func resizePanel(for ids: [String]) {
        panel?.place(canvas: CorralLayout.layout(rankedIDs: ids).canvas)
        refreshCards()
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

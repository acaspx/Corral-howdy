// CorralGlassView.swift
// SwiftUI view for the corral: liquid-glass shape + agent tokens + interactions.
// State comes from the Rust core (via the CorralNative C ABI); this view only displays it
// and sends intents back. No policy or lifecycle decisions here (AGENTS.md rule 3).
//
// Status: SPIKE S5 prototype. Builds in CI; visual tuning pending. Requires macOS 26 (system Liquid Glass).

import SwiftUI

public struct CorralAgentToken: Identifiable, Equatable, Sendable {
    public enum Status: Equatable, Sendable { case available, working, needsYou, done, error }
    public var id: String
    public var name: String
    public var monogram: String       // e.g. "Gk", or an SF Symbol name via `symbol`
    public var symbol: String?
    public var status: Status
    /// The provider's logo in Corral's logos folder, already checked by `CorralLogo.isValidPath`
    /// (ADR-015). Nil: the drop shows its monogram.
    public var logoPath: String?

    public init(id: String, name: String, monogram: String, symbol: String? = nil, status: Status = .available,
                logoPath: String? = nil) {
        self.id = id; self.name = name; self.monogram = monogram; self.symbol = symbol; self.status = status
        self.logoPath = logoPath
    }
}

public enum CorralIntent: Equatable, Sendable {
    case deploy(agentIDs: [String])
    case openAgent(id: String)
    case summonPromptBar
    /// A task typed into a drop's prompt field (SK-6). Rust checks the prompt.
    case deployTask(id: String, prompt: String)
    /// Stop, Send back and Retry on a run card.
    case stop(id: String)
    case sendBack(id: String)
    case retry(id: String)
}

/// Springs drops between layouts so the shape morphs instead of jumping.
/// Works in polar coordinates around the shape's pivot: when usage reorders the ring,
/// drops slide AROUND the ring (shortest arc) instead of cutting across its hollow centre.
@MainActor
final class CorralMorph: ObservableObject {
    struct Polar { var angle: CGFloat; var distance: CGFloat }
    @Published private(set) var blobs: [CorralBlob] = []
    private var targets: [String: CorralBlob] = [:]
    private var vel: [String: (da: CGFloat, dd: CGFloat)] = [:]
    private var lastFrame: Date?
    private(set) var isSettling = false

    private let stiffness: CGFloat = 190, damping: CGFloat = 24   // response ≈ 0.45 s
    /// Longest step the spring takes. After a stall (sleep, a busy main thread) the morph resumes
    /// smoothly instead of jumping or blowing up (TP-SHAPE-11).
    private let maxStep: CGFloat = 1.0 / 30.0

    static func polar(_ p: CGPoint) -> Polar { Polar(angle: atan2(p.y, p.x), distance: hypot(p.x, p.y)) }
    static func point(_ q: Polar) -> CGPoint { CGPoint(x: q.distance * cos(q.angle), y: q.distance * sin(q.angle)) }
    static func shortestArc(from a: CGFloat, to b: CGFloat) -> CGFloat {
        var d = (b - a).truncatingRemainder(dividingBy: 2 * .pi)
        if d > .pi { d -= 2 * .pi } else if d < -.pi { d += 2 * .pi }
        return d
    }

    func setTarget(_ newBlobs: [CorralBlob], reduceMotion: Bool) {
        // Keep the first of any duplicate IDs: this data crosses the C ABI (TP-SHAPE-09).
        var seen = Set<String>()
        let newBlobs = newBlobs.filter { seen.insert($0.id).inserted }
        targets = Dictionary(newBlobs.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        lastFrame = nil
        if reduceMotion || blobs.isEmpty {
            blobs = newBlobs; vel = [:]; isSettling = false; return
        }
        // Removed drops detach and shrink away; new drops grow from a bead (docs/07 §7).
        var merged = blobs.map { b -> CorralBlob in
            guard targets[b.id] == nil else { return b }
            var t = b; t.attach = 0; t.radius = 0.01; t.isRingMember = false
            t.offset.y -= 120; targets[b.id] = t
            return b
        }
        for nb in newBlobs where !merged.contains(where: { $0.id == nb.id }) {
            var start = nb; start.attach = 0; start.radius = nb.radius * 0.2
            start.offset = CGPoint(x: nb.offset.x * 0.2, y: nb.offset.y * 0.2)
            merged.append(start)
        }
        blobs = merged
        isSettling = true
    }

    /// Advances the morph to `now` using real elapsed time, so it feels the same at 60 Hz and on
    /// 120 Hz ProMotion displays (TP-SHAPE-10). The first call after a new target only records the time.
    func advance(to now: Date) {
        defer { lastFrame = now }
        guard let last = lastFrame else { return }
        step(dt: min(max(CGFloat(now.timeIntervalSince(last)), 0), maxStep))
    }

    func step(dt: CGFloat) {
        guard isSettling, dt > 0 else { return }
        var moving = false
        blobs = blobs.compactMap { b in
            guard let t = targets[b.id] else { return nil }
            let cur = Self.polar(b.offset), dst = Self.polar(t.offset)
            var v = vel[b.id] ?? (0, 0)
            // Angle is meaningless at the centre; let distance lead there.
            let dA = (cur.distance < 1 || dst.distance < 1) ? 0 : Self.shortestArc(from: cur.angle, to: dst.angle)
            let dD = dst.distance - cur.distance
            v.da += (stiffness * dA - damping * v.da) * dt
            v.dd += (stiffness * dD - damping * v.dd) * dt
            var nb = b
            let angle = (cur.distance < 1) ? dst.angle : cur.angle + v.da * dt
            nb.offset = Self.point(Polar(angle: angle, distance: cur.distance + v.dd * dt))
            nb.radius += (t.radius - b.radius) * (1 - exp(-dt * 9))   // exact exponential ease:
            nb.attach += (t.attach - b.attach) * (1 - exp(-dt * 7))   // same result at any frame rate
            nb.isRingMember = t.isRingMember
            nb.rank = t.rank
            vel[b.id] = v
            let settled = abs(dA) < 0.002 && abs(dD) < 0.2 && abs(t.radius - nb.radius) < 0.2
                && abs(v.da) < 0.01 && abs(v.dd) < 1
            if !settled { moving = true }
            if t.radius < 0.02 && nb.radius < 0.5 { return nil }   // fully pinched-off leaver
            return nb
        }
        isSettling = moving
    }
}

public struct CorralGlassView: View {
    /// Drops to show: agents in the corral, already filtered to the user's chosen set (≤ 8)
    /// and sorted most-used first by the Rust core. Rank 1 lands top-left, then clockwise.
    public var agents: [CorralAgentToken]
    /// Agents that exist but are not shown (more than 8 configured). Spoken by VoiceOver only.
    public var hiddenCount: Int
    public var onIntent: (CorralIntent) -> Void
    /// The pointer entered (true) or left (false) a drop. Drives the hover card (Spike S6).
    public var onHoverDrop: ((String, Bool) -> Void)?

    @StateObject private var morph = CorralMorph()
    @State private var selection: Set<String> = []
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    private let metrics = CorralMetrics()
    private let start = Date()

    public init(agents: [CorralAgentToken], hiddenCount: Int = 0, onIntent: @escaping (CorralIntent) -> Void,
                onHoverDrop: ((String, Bool) -> Void)? = nil) {
        self.agents = agents; self.hiddenCount = hiddenCount; self.onIntent = onIntent
        self.onHoverDrop = onHoverDrop
    }

    public var body: some View {
        let target = CorralLayout.layout(rankedIDs: agents.map(\.id), metrics: metrics)

        // Only tick frames while morphing or animating; idle corral costs ~0 CPU/GPU.
        TimelineView(.animation(paused: !morph.isSettling)) { context in
            let t = reduceMotion ? 0 : Float(context.date.timeIntervalSince(start))
            let payload = CorralLayout.shaderPayload(morph.blobs, pivot: target.pivot)

            ZStack(alignment: .topLeading) {
                if reduceTransparency {
                    solidFallback(target.canvas, pivot: target.pivot)
                } else if let shaders = CorralShaders.library {
                    // macOS 26: the system Liquid Glass material supplies real refraction of whatever
                    // is behind the corral. The Metal pass carves it into drops + bridges and adds
                    // rim, highlights and caustics. How the two layers compose is Spike S5's job;
                    // this is the starting arrangement (docs/07 §4).
                    Rectangle()
                        .fill(.clear)
                        .glassEffect(.regular, in: .rect)
                        .frame(width: target.canvas.width, height: target.canvas.height)
                        .layerEffect(
                            shaders.corralGlass(
                                .float2(target.canvas),
                                .float(t),
                                .floatArray(payload.values),
                                .float(Float(payload.ringCount)),
                                .color(Color(red: 0.62, green: 0.72, blue: 0.82)),   // brand glass tint
                                .float4(14, 12, 0.35, 9)                             // refract, bevel, frost, neck
                            ),
                            maxSampleOffset: CGSize(width: 20, height: 20)
                        )
                } else {
                    plainGlass(target.canvas, pivot: target.pivot)
                }
                ForEach(morph.blobs.filter { b in agents.contains { $0.id == b.id } }) { blob in
                    if let agent = agents.first(where: { $0.id == blob.id }) {
                        token(agent, blob: blob, pivot: target.pivot)
                    }
                }
            }
            .frame(width: target.canvas.width, height: target.canvas.height)
            .onChange(of: context.date) { _, now in morph.advance(to: now) }
        }
        .onAppear { morph.setTarget(target.blobs, reduceMotion: true) }
        .onChange(of: agents.map(\.id)) { _, _ in
            // Fires on deploy/return AND on usage reorders (same IDs, new order).
            morph.setTarget(CorralLayout.layout(rankedIDs: agents.map(\.id), metrics: metrics).blobs,
                            reduceMotion: reduceMotion)
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(accessibilitySummary)
    }

    @ViewBuilder
    private func token(_ agent: CorralAgentToken, blob: CorralBlob, pivot: CGPoint) -> some View {
        let selected = selection.contains(agent.id)
        Button {
            if NSEvent.modifierFlags.contains(.command) {
                if selected { selection.remove(agent.id) } else { selection.insert(agent.id) }
            } else {
                onIntent(.openAgent(id: agent.id))
            }
        } label: {
            Group {
                if let path = agent.logoPath, let logo = CorralLogoCache.shared.image(at: path) {
                    // 46% of the drop, above the glass, full color (docs/07 §8.1).
                    Image(decorative: logo, scale: 1)
                        .resizable()
                        .interpolation(.high)
                        .aspectRatio(contentMode: .fit)
                        .frame(width: blob.radius * 2 * 0.46, height: blob.radius * 2 * 0.46)
                        .clipShape(Circle())
                } else if let s = agent.symbol {
                    Image(systemName: s)
                } else {
                    Text(agent.monogram)
                }
            }
            .font(.system(size: blob.radius * 0.55, weight: .semibold, design: .rounded))
            .foregroundStyle(.primary.opacity(0.75))
            .frame(width: blob.radius * 2, height: blob.radius * 2)
            .overlay(statusRing(agent.status, radius: blob.radius, selected: selected))
            .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .onHover { inside in onHoverDrop?(agent.id, inside) }
        .position(x: pivot.x + blob.offset.x, y: pivot.y + blob.offset.y)
        .accessibilityLabel("\(agent.name), \(statusText(agent.status)), number \(blob.rank) by use")
        .accessibilityHint("Opens the agent. Command-click to select for group deploy.")
    }

    @ViewBuilder
    private func statusRing(_ s: CorralAgentToken.Status, radius: CGFloat, selected: Bool) -> some View {
        // Status is never colour-only: every mark has a shape (CorralStatusMark, docs/07 §8.1).
        ZStack(alignment: .topTrailing) {
            Circle()
                .inset(by: 3)
                .stroke(selected ? Color.accentColor : .clear, lineWidth: 2)
            switch CorralStatusMark.of(s) {
            case .none:
                EmptyView()
            case .turningArc:
                // Draws frames only while the agent works, so an idle corral stays at 0 frames.
                TimelineView(.animation(minimumInterval: 1.0 / 30.0, paused: reduceMotion)) { context in
                    let turn = context.date.timeIntervalSinceReferenceDate.truncatingRemainder(dividingBy: 2.4) / 2.4
                    Circle()
                        .inset(by: 2)
                        .trim(from: 0, to: 0.28)
                        .stroke(Self.coral, style: StrokeStyle(lineWidth: 3, lineCap: .round))
                        .rotationEffect(.degrees(reduceMotion ? -90 : turn * 360))
                }
            case .pulse(let badge):
                TimelineView(.animation(minimumInterval: 1.0 / 30.0, paused: reduceMotion)) { context in
                    let phase = reduceMotion ? 1 : (sin(context.date.timeIntervalSinceReferenceDate * 3) + 1) / 2
                    Circle()
                        .inset(by: 2)
                        .stroke(Self.maroon.opacity(0.55 + 0.45 * phase), lineWidth: 2.5)
                }
                badgeView(badge, tint: Self.maroon)
            case .badge(let symbol):
                badgeView(symbol, tint: s == .error ? Self.maroon : Self.ink)
            }
        }
    }

    private func badgeView(_ symbol: String, tint: Color) -> some View {
        Image(systemName: symbol)
            .font(.system(size: 10, weight: .bold))
            .foregroundStyle(tint)
            .frame(width: 20, height: 20)
            .background(Circle().fill(Color.white))
            .overlay(Circle().stroke(tint.opacity(0.25)))
    }

    static let coral = Color(red: 0.91, green: 0.42, blue: 0.33)
    static let maroon = Color(red: 0.48, green: 0.12, blue: 0.16)
    static let ink = Color(red: 0.11, green: 0.10, blue: 0.10)

    private func plainGlass(_ canvas: CGSize, pivot: CGPoint) -> some View {
        // No compiled shader found (CorralShaders): plain system glass drops, no bridges.
        ZStack(alignment: .topLeading) {
            ForEach(morph.blobs) { b in
                Circle()
                    .fill(.clear)
                    .glassEffect(.regular, in: .circle)
                    .frame(width: b.radius * 2, height: b.radius * 2)
                    .position(x: pivot.x + b.offset.x, y: pivot.y + b.offset.y)
            }
        }
        .frame(width: canvas.width, height: canvas.height)
    }

    private func solidFallback(_ canvas: CGSize, pivot: CGPoint) -> some View {
        // Reduce Transparency: same geometry, opaque frosted fill (drawn with SwiftUI shapes).
        Canvas { ctx, _ in
            for b in morph.blobs {
                let c = CGPoint(x: pivot.x + b.offset.x, y: pivot.y + b.offset.y)
                ctx.fill(Path(ellipseIn: CGRect(x: c.x - b.radius, y: c.y - b.radius,
                                                width: b.radius * 2, height: b.radius * 2)),
                         with: .color(Color(nsColor: .windowBackgroundColor)))
            }
        }
        .frame(width: canvas.width, height: canvas.height)
    }

    private var accessibilitySummary: String {
        let shown: String
        switch agents.count {
        case 0: shown = "Corral, empty"
        case 1: shown = "Corral, 1 agent available"
        default: shown = "Corral, \(agents.count) agents available, most used first"
        }
        return hiddenCount > 0 ? "\(shown). \(hiddenCount) more not shown" : shown
    }

    private func statusText(_ s: CorralAgentToken.Status) -> String {
        switch s {
        case .available: "available"
        case .working: "working"
        case .needsYou: "needs your approval"
        case .done: "done"
        case .error: "error"
        }
    }
}

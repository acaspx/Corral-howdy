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
}

public enum CorralIntent: Sendable {
    case deploy(agentIDs: [String])
    case openAgent(id: String)
    case summonPromptBar
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
    private(set) var isSettling = false

    private let stiffness: CGFloat = 190, damping: CGFloat = 24   // response ≈ 0.45 s

    static func polar(_ p: CGPoint) -> Polar { Polar(angle: atan2(p.y, p.x), distance: hypot(p.x, p.y)) }
    static func point(_ q: Polar) -> CGPoint { CGPoint(x: q.distance * cos(q.angle), y: q.distance * sin(q.angle)) }
    static func shortestArc(from a: CGFloat, to b: CGFloat) -> CGFloat {
        var d = (b - a).truncatingRemainder(dividingBy: 2 * .pi)
        if d > .pi { d -= 2 * .pi } else if d < -.pi { d += 2 * .pi }
        return d
    }

    func setTarget(_ newBlobs: [CorralBlob], reduceMotion: Bool) {
        targets = Dictionary(uniqueKeysWithValues: newBlobs.map { ($0.id, $0) })
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

    func step(dt: CGFloat) {
        guard isSettling else { return }
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
            nb.radius += (t.radius - b.radius) * min(1, dt * 9)
            nb.attach += (t.attach - b.attach) * min(1, dt * 7)
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

    @StateObject private var morph = CorralMorph()
    @State private var selection: Set<String> = []
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    private let metrics = CorralMetrics()
    private let start = Date()

    public init(agents: [CorralAgentToken], hiddenCount: Int = 0, onIntent: @escaping (CorralIntent) -> Void) {
        self.agents = agents; self.hiddenCount = hiddenCount; self.onIntent = onIntent
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
                } else {
                    // macOS 26: the system Liquid Glass material supplies real refraction of whatever
                    // is behind the corral. The Metal pass carves it into drops + bridges and adds
                    // rim, highlights and caustics. How the two layers compose is Spike S5's job;
                    // this is the starting arrangement (docs/07 §4).
                    Rectangle()
                        .fill(.clear)
                        .glassEffect(.regular, in: .rect)
                        .frame(width: target.canvas.width, height: target.canvas.height)
                        .layerEffect(
                            ShaderLibrary.bundle(.module).corralGlass(
                                .float2(target.canvas),
                                .float(t),
                                .floatArray(payload.values),
                                .float(Float(payload.ringCount)),
                                .color(Color(red: 0.62, green: 0.72, blue: 0.82)),   // brand glass tint
                                .float4(14, 12, 0.35, 9)                             // refract, bevel, frost, neck
                            ),
                            maxSampleOffset: CGSize(width: 20, height: 20)
                        )
                }
                ForEach(morph.blobs.filter { b in agents.contains { $0.id == b.id } }) { blob in
                    if let agent = agents.first(where: { $0.id == blob.id }) {
                        token(agent, blob: blob, pivot: target.pivot)
                    }
                }
            }
            .frame(width: target.canvas.width, height: target.canvas.height)
            .onChange(of: context.date) { _, _ in morph.step(dt: 1.0 / 60.0) }
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
                if let s = agent.symbol { Image(systemName: s) } else { Text(agent.monogram) }
            }
            .font(.system(size: blob.radius * 0.55, weight: .semibold, design: .rounded))
            .foregroundStyle(.primary.opacity(0.75))
            .frame(width: blob.radius * 2, height: blob.radius * 2)
            .overlay(statusRing(agent.status, radius: blob.radius, selected: selected))
            .contentShape(Circle())
        }
        .buttonStyle(.plain)
        .position(x: pivot.x + blob.offset.x, y: pivot.y + blob.offset.y)
        .accessibilityLabel("\(agent.name), \(statusText(agent.status)), number \(blob.rank) by use")
        .accessibilityHint("Opens the agent. Command-click to select for group deploy.")
    }

    private func statusRing(_ s: CorralAgentToken.Status, radius: CGFloat, selected: Bool) -> some View {
        // Status is never colour-only: needsYou also pulses and adds a badge glyph.
        Circle()
            .inset(by: 3)
            .stroke(selected ? Color.accentColor : .clear, lineWidth: 2)
            .overlay(alignment: .topTrailing) {
                if s == .needsYou {
                    Image(systemName: "hand.raised.fill").font(.caption2).padding(4)
                }
            }
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

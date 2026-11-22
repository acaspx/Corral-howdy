// CorralTraceView.swift
// The trace tree (docs/07 §8.3): a column per running task, each a prompt pill on dark glass, a
// hairline stem, the task card, another stem and the subtask stack, with a 1 pt wire into the
// agent's drop that turns maroon while it waits on you. "+N more running" sits beside them.

import SwiftUI

@MainActor
final class CorralTraceModel: ObservableObject {
    @Published var trace = CorralTrace.empty
    /// Each column's wire end, in the view's own x.
    @Published var anchors: [String: CGFloat] = [:]
    @Published var width: CGFloat = 0
    @Published var wires = true
}

struct CorralTraceRoot: View {
    @ObservedObject var model: CorralTraceModel

    static let ink = CorralCardView.ink
    static let maroon = CorralCardView.maroon

    var body: some View {
        let columns = model.trace.columns
        VStack(spacing: 0) {
            HStack(alignment: .bottom, spacing: CorralTraceLayout.columnGap) {
                ForEach(columns, id: \.id) { column in
                    CorralTraceColumnView(column: column)
                }
            }
            .overlay(alignment: .topTrailing) {
                if model.trace.more > 0 {
                    Text("+\(model.trace.more) more running")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.white)
                        .padding(.horizontal, 8)
                        .padding(.vertical, 3)
                        .background(Capsule().fill(Color.black.opacity(0.55)))
                        .offset(x: 12, y: -10)
                }
            }
            .frame(width: model.width)
            if model.wires {
                wires(columns)
                    .frame(width: model.width, height: CorralTraceLayout.wireHeight)
            }
        }
        .transaction { $0.animation = nil }
    }

    private func wires(_ columns: [CorralTrace.Column]) -> some View {
        let centers = CorralTraceLayout.columnCenters(count: columns.count, width: model.width)
        return ZStack {
            ForEach(Array(zip(columns, centers)), id: \.0.id) { column, x in
                let end = model.anchors[column.id] ?? x
                Self.path(CorralTraceLayout.wire(from: CGPoint(x: x, y: 0),
                                                 to: CGPoint(x: end, y: CorralTraceLayout.wireHeight)))
                    .stroke(column.status == .needsYou ? Self.maroon : Self.ink.opacity(0.45), lineWidth: 1)
            }
        }
        .accessibilityHidden(true)
    }

    /// The wire through its corners, rounded with 12 pt elbows.
    static func path(_ corners: [CGPoint]) -> Path {
        Path { path in
            guard let first = corners.first else { return }
            path.move(to: first)
            for i in corners.indices.dropFirst() {
                if i < corners.count - 1 {
                    path.addArc(tangent1End: corners[i], tangent2End: corners[i + 1],
                                radius: CorralTraceLayout.elbow)
                } else {
                    path.addLine(to: corners[i])
                }
            }
        }
    }
}

struct CorralTraceColumnView: View {
    let column: CorralTrace.Column

    static let ink = CorralCardView.ink
    static let maroon = CorralCardView.maroon

    var body: some View {
        VStack(spacing: 0) {
            promptPill
            stem
            taskCard
            if let subtask = column.subtask {
                stem
                subtaskStack(subtask)
            }
        }
        .frame(width: CorralTraceLayout.columnWidth)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Self.label(column))
    }

    private var promptPill: some View {
        Text("“\(column.prompt)\(column.promptTruncated ? "…" : "")”")
            .font(.system(size: 12))
            .foregroundStyle(.white)
            .lineLimit(2)
            .multilineTextAlignment(.center)
            .padding(.horizontal, 12)
            .padding(.vertical, 7)
            .glassEffect(.regular.tint(Color.black.opacity(0.45)), in: .capsule)
    }

    private var stem: some View {
        Rectangle().fill(Self.ink.opacity(0.3)).frame(width: 1, height: 10)
    }

    private var taskCard: some View {
        HStack(spacing: 8) {
            logo
            VStack(alignment: .leading, spacing: 2) {
                Text(column.name ?? "Agent")
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(Self.ink)
                    .lineLimit(1)
                Text(Self.statusText(column.status))
                    .font(.caption)
                    .foregroundStyle(column.status == .needsYou ? Self.maroon : Self.ink.opacity(0.6))
            }
            Spacer(minLength: 0)
        }
        .padding(10)
        .frame(maxWidth: .infinity)
        .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 14, style: .continuous))
    }

    @ViewBuilder
    private var logo: some View {
        if let path = column.logoPath, let image = CorralLogoCache.shared.image(at: path) {
            Image(decorative: image, scale: 1)
                .resizable()
                .interpolation(.high)
                .frame(width: 22, height: 22)
                .clipShape(RoundedRectangle(cornerRadius: 5))
        } else {
            Text(String((column.name ?? column.id).prefix(1)).uppercased())
                .font(.system(size: 11, weight: .semibold))
                .foregroundStyle(Self.ink)
                .frame(width: 22, height: 22)
                .background(RoundedRectangle(cornerRadius: 5).fill(Self.ink.opacity(0.08)))
        }
    }

    /// The current subtask on top; up to 2 more peek out below it.
    private func subtaskStack(_ subtask: CorralTrace.Subtask) -> some View {
        ZStack(alignment: .top) {
            ForEach(Array(column.next.enumerated().reversed()), id: \.offset) { index, _ in
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .fill(Color.white.opacity(0.35))
                    .frame(height: 54)
                    .padding(.horizontal, CGFloat(index + 1) * 6)
                    .offset(y: CGFloat(index + 1) * 6)
            }
            VStack(alignment: .leading, spacing: 3) {
                Text("SUBTASK \(subtask.step) OF \(subtask.total)")
                    .font(.caption2.weight(.semibold))
                    .tracking(1.1)
                    .foregroundStyle(Self.maroon)
                Text(subtask.title)
                    .font(.system(size: 12, weight: .semibold))
                    .foregroundStyle(Self.ink)
                    .lineLimit(1)
            }
            .padding(10)
            .frame(maxWidth: .infinity, minHeight: 54, alignment: .leading)
            .glassEffect(.regular, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        }
        .padding(.bottom, CGFloat(column.next.count) * 6)
    }

    nonisolated static func statusText(_ status: CorralTrace.Status) -> String {
        switch status {
        case .queued: "Waiting to start"
        case .working: "Working"
        case .needsYou: "Needs you"
        }
    }

    /// What VoiceOver reads for a column.
    nonisolated static func label(_ column: CorralTrace.Column) -> String {
        var label = "\(column.name ?? "Agent"), \(statusText(column.status)): \(column.prompt)."
        if let subtask = column.subtask {
            label += " Subtask \(subtask.step) of \(subtask.total): \(subtask.title)."
        }
        return label
    }
}

// CorralTraceLayout.swift
// Where the trace tree goes (docs/07 §8.3, TP-TRACE-02..04): columns side by side above the
// corral (or above the open prompt bar), and each column's wire into its agent's drop. Pure, so
// tests need no window. The core chose the columns; this only places them.

import CoreGraphics

public enum CorralTraceLayout {
    public static let columnWidth: CGFloat = 220
    public static let columnGap: CGFloat = 16
    /// The space under the columns where the wires run down to the corral.
    public static let wireHeight: CGFloat = 28
    /// The rounded corner where a wire turns.
    public static let elbow: CGFloat = 12
    /// A hidden agent has no drop: its wire ends this far in from the corral's right end, where
    /// the hidden bead goes.
    public static let hiddenInset: CGFloat = 24
    static let margin: CGFloat = 8

    /// Each shown drop's center x on screen, from the corral's layout: right after a usage
    /// reorder or a shape morph, the same as the corral's (TP-TRACE-03).
    public static func dropXs(rankedIDs: [String], corralMinX: CGFloat) -> [String: CGFloat] {
        let layout = CorralLayout.layout(rankedIDs: rankedIDs)
        var xs: [String: CGFloat] = [:]
        for blob in layout.blobs where rankedIDs.contains(blob.id) {
            xs[blob.id] = corralMinX + layout.pivot.x + blob.offset.x
        }
        return xs
    }

    /// Where a column's wire ends: its drop, or for a hidden agent, the corral's right end.
    public static func anchorX(for id: String, drops: [String: CGFloat], corral: CGRect) -> CGFloat {
        drops[id] ?? corral.maxX - hiddenInset
    }

    /// The columns' centers across a view `width` wide, side by side and centered.
    public static func columnCenters(count: Int, width: CGFloat) -> [CGFloat] {
        guard count > 0 else { return [] }
        let step = columnWidth + columnGap
        let first = width / 2 - CGFloat(count - 1) * step / 2
        return (0..<count).map { first + CGFloat($0) * step }
    }

    /// The tree's window: from `above` (the corral's top, or the open prompt bar's) up, centered
    /// on the corral, wide enough for the columns and every drop's wire, kept on screen.
    public static func frame(columns: Int, height: CGFloat, corral: CGRect, above: CGFloat,
                             visible: CGRect) -> CGRect {
        let span = CGFloat(columns) * columnWidth + CGFloat(max(columns - 1, 0)) * columnGap
        let width = min(max(span, corral.width), visible.width - 2 * margin)
        let x = min(max(corral.midX - width / 2, visible.minX + margin), visible.maxX - width - margin)
        let fits = min(height, visible.maxY - margin - above)
        return CGRect(x: x, y: above, width: width, height: max(fits, 0))
    }

    /// A wire's corners, top to bottom (y grows down): down from the column, across halfway, and
    /// down to the drop. Too close for two elbows, it's straight.
    public static func wire(from start: CGPoint, to end: CGPoint) -> [CGPoint] {
        guard abs(end.x - start.x) >= 2 * elbow else { return [start, end] }
        let mid = (start.y + end.y) / 2
        return [start, CGPoint(x: start.x, y: mid), CGPoint(x: end.x, y: mid), end]
    }

    /// A click on `id`'s subtask stack: fans it out, or folds it back (TP-TRACE-05).
    public static func toggled(_ fanned: Set<String>, _ id: String) -> Set<String> {
        fanned.contains(id) ? fanned.subtracting([id]) : fanned.union([id])
    }

    /// The fanned stacks that still have a column.
    public static func kept(_ fanned: Set<String>, columns: [String]) -> Set<String> {
        fanned.intersection(columns)
    }

    /// Nothing running: no tree, so the corral stays calm (TP-TRACE-06).
    public static func shows(_ trace: CorralTrace) -> Bool { !trace.columns.isEmpty }

    /// The open prompt bar sits between the columns and the corral, so the wires hide meanwhile.
    public static func showsWires(barOpen: Bool) -> Bool { !barOpen }
}

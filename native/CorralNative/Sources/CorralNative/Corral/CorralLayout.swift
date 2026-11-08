// CorralLayout.swift
// Pure geometry for the corral's shape grammar and usage-ranked slot order.
// No AppKit, no Metal, so it is fully unit-testable (Tests/CorralNativeTests/CorralLayoutTests.swift).
//
// Shape grammar (docs/07-brand-and-corral-surface.md §3):
//   0 agents  → empty puddle
//   1         → circle
//   2         → dumbbell (two drops, one bridge)
//   3…8       → regular polygon ring with a hollow centre, flat bottom edge
//   The corral never shows more than 8 drops (an octagon). With more than 8 agents, the user
//   chooses which ones show (CorralRoster / Rust core); this layer only ever receives ≤ 8.
//
// Ordering (§3.1): input IDs arrive in usage-rank order from the Rust core (most used first).
// Rank 1 sits in the top-left slot, and ranks continue clockwise.
//
// Coordinates are SwiftUI points, y pointing down. Offsets are relative to the shape's centre
// (the pivot), so the morph can spring in polar coordinates and reorders travel around the ring.

import CoreGraphics
import Foundation

public enum CorralShape: Equatable, Sendable {
    case empty
    case circle
    case dumbbell
    case polygon(sides: Int)
}

public struct CorralBlob: Equatable, Sendable, Identifiable {
    public var id: String          // agent id, or "corral.puddle.*"
    public var offset: CGPoint     // centre relative to the shape pivot
    public var radius: CGFloat
    /// 1 = fully attached to its neighbours, 0 = detached (add/remove animation).
    public var attach: CGFloat
    /// Whether this blob takes part in ring bridges.
    public var isRingMember: Bool
    /// 1-based usage rank among the drops currently shown (0 for puddle blobs).
    public var rank: Int
}

public struct CorralMetrics: Equatable, Sendable {
    public var blobRadius: CGFloat = 34       // ~68 pt agent tokens
    public var gap: CGFloat = 40              // bridge length between blob edges (tuned in Spike S5 render)
    public var padding: CGFloat = 28          // room for shadow + refraction overshoot
    public var bridgeWidthRatio: CGFloat = 0.22 // bridge radius / blob radius
    public static let maxShown = 8            // octagon
    public init() {}
}

public enum CorralLayout {

    public static func shape(forShownCount n: Int) -> CorralShape {
        switch n {
        case ..<1: return .empty
        case 1: return .circle
        case 2: return .dumbbell
        default: return .polygon(sides: min(n, CorralMetrics.maxShown))
        }
    }

    /// Circumradius of a ring of `sides` blobs whose edge-to-edge gap is `m.gap`.
    public static func ringRadius(sides: Int, _ m: CorralMetrics) -> CGFloat {
        precondition(sides >= 2)
        return (2 * m.blobRadius + m.gap) / (2 * sin(.pi / CGFloat(sides)))
    }

    /// Vertex angles in increasing order. y-down space: 0 = right, π/2 = down, so increasing
    /// angle is CLOCKWISE on screen. Offset by π/n so an edge (not a vertex) sits at the bottom.
    public static func ringAngles(sides: Int) -> [CGFloat] {
        let n = CGFloat(sides)
        return (0..<sides).map { k in .pi / 2 + .pi / n + 2 * .pi * CGFloat(k) / n }
    }

    public static func ringOffsets(sides: Int, _ m: CorralMetrics) -> [CGPoint] {
        let r = ringRadius(sides: sides, m)
        return ringAngles(sides: sides).map { CGPoint(x: r * cos($0), y: r * sin($0)) }
    }

    /// Index (into ringAngles) of the slot that holds rank 1: the vertex pointing most
    /// towards the top-left (angle 5π/4 in y-down space). Ties (the octagon's two top-left
    /// vertices) go to the higher one, i.e. the left end of the top edge.
    public static func topLeftSlot(sides: Int) -> Int {
        let target = 5 * CGFloat.pi / 4
        let angles = ringAngles(sides: sides)
        func dist(_ a: CGFloat) -> CGFloat {
            let d = abs((a - target).truncatingRemainder(dividingBy: 2 * .pi))
            return min(d, 2 * .pi - d)
        }
        return angles.indices.min { i, j in
            let di = dist(angles[i]), dj = dist(angles[j])
            if abs(di - dj) > 1e-6 { return di < dj }
            return sin(angles[i]) < sin(angles[j])        // smaller y = higher on screen
        }!
    }

    /// Slot index for each rank (0-based rank → index into ringOffsets), clockwise from top-left.
    public static func slotOrder(sides: Int) -> [Int] {
        let start = topLeftSlot(sides: sides)
        return (0..<sides).map { (start + $0) % sides }
    }

    /// Full layout. `rankedIDs` = drops to show, most used first (≤ 8; extras are ignored).
    /// Duplicate IDs are dropped (the first, highest-ranked one wins): this input comes from
    /// across the C ABI, and the view must never trap on it (TP-SHAPE-08).
    public static func layout(rankedIDs: [String], metrics m: CorralMetrics = .init())
        -> (blobs: [CorralBlob], canvas: CGSize, pivot: CGPoint, shape: CorralShape)
    {
        var seen = Set<String>()
        let ids = Array(rankedIDs.filter { seen.insert($0).inserted }.prefix(CorralMetrics.maxShown))
        let shape = shape(forShownCount: ids.count)
        var blobs: [CorralBlob] = []

        switch shape {
        case .empty:
            let r = m.blobRadius * 0.55
            blobs = [CorralBlob(id: "corral.puddle.a", offset: CGPoint(x: -r * 0.7, y: 0), radius: r, attach: 1, isRingMember: false, rank: 0),
                     CorralBlob(id: "corral.puddle.b", offset: CGPoint(x:  r * 0.7, y: 0), radius: r, attach: 1, isRingMember: false, rank: 0)]
        case .circle:
            blobs = [CorralBlob(id: ids[0], offset: .zero, radius: m.blobRadius, attach: 1, isRingMember: false, rank: 1)]
        case .dumbbell:
            // Rank 1 on the left, rank 2 on the right (clockwise from top-left).
            let half = m.blobRadius + m.gap / 2
            blobs = [CorralBlob(id: ids[0], offset: CGPoint(x: -half, y: 0), radius: m.blobRadius, attach: 1, isRingMember: true, rank: 1),
                     CorralBlob(id: ids[1], offset: CGPoint(x:  half, y: 0), radius: m.blobRadius, attach: 1, isRingMember: true, rank: 2)]
        case .polygon(let sides):
            let offsets = ringOffsets(sides: sides, m)
            // Emitted in rank order. Consecutive ranks are clockwise neighbours, so the shader's
            // i → i+1 (mod n) bridges still trace the ring.
            blobs = slotOrder(sides: sides).enumerated().map { rank, slot in
                CorralBlob(id: ids[rank], offset: offsets[slot], radius: m.blobRadius,
                           attach: 1, isRingMember: true, rank: rank + 1)
            }
        }

        let minX = blobs.map { $0.offset.x - $0.radius }.min() ?? 0
        let maxX = blobs.map { $0.offset.x + $0.radius }.max() ?? 0
        let minY = blobs.map { $0.offset.y - $0.radius }.min() ?? 0
        let maxY = blobs.map { $0.offset.y + $0.radius }.max() ?? 0
        let canvas = CGSize(width: (maxX - minX) + 2 * m.padding, height: (maxY - minY) + 2 * m.padding)
        let pivot = CGPoint(x: m.padding - minX, y: m.padding - minY)
        return (blobs, canvas, pivot, shape)
    }

    /// Packs blobs for the Metal shader: [cx, cy, radius, attach] per blob in canvas space.
    /// Ring members come first, in ring order, so the shader can bridge i → i+1 (mod ringCount).
    public static func shaderPayload(_ blobs: [CorralBlob], pivot: CGPoint) -> (values: [Float], ringCount: Int) {
        let ring = blobs.filter(\.isRingMember).sorted { $0.rank < $1.rank }
        let rest = blobs.filter { !$0.isRingMember }
        let values = (ring + rest).flatMap {
            [Float(pivot.x + $0.offset.x), Float(pivot.y + $0.offset.y), Float($0.radius), Float($0.attach)]
        }
        return (values, ring.count)
    }
}

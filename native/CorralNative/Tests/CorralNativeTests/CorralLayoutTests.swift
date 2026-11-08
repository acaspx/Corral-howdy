// CorralLayoutTests.swift: TP-SHAPE-* and TP-RANK-* (docs/04-test-plan.md)
import Testing
import CoreGraphics
@testable import CorralNative

private func ids(_ n: Int) -> [String] { (0..<n).map { "agent-\($0)" } }   // already rank-ordered

@Test("TP-SHAPE-01 shape grammar by shown count, capped at an octagon")
func shapeGrammar() {
    #expect(CorralLayout.shape(forShownCount: 0) == .empty)
    #expect(CorralLayout.shape(forShownCount: 1) == .circle)
    #expect(CorralLayout.shape(forShownCount: 2) == .dumbbell)
    #expect(CorralLayout.shape(forShownCount: 3) == .polygon(sides: 3))
    #expect(CorralLayout.shape(forShownCount: 4) == .polygon(sides: 4))
    #expect(CorralLayout.shape(forShownCount: 8) == .polygon(sides: 8))
    #expect(CorralLayout.shape(forShownCount: 11) == .polygon(sides: 8))
}

@Test("TP-SHAPE-02 polygons sit on a flat bottom edge", arguments: 3...8)
func flatBottom(sides: Int) {
    let pts = CorralLayout.ringOffsets(sides: sides, CorralMetrics())
    let maxY = pts.map(\.y).max()!
    #expect(pts.filter { abs($0.y - maxY) < 0.001 }.count == 2)
}

@Test("TP-SHAPE-03 triangle points up, square is axis-aligned")
func orientation() {
    let tri = CorralLayout.ringOffsets(sides: 3, CorralMetrics())
    #expect(tri.contains { abs($0.x) < 0.001 && $0.y < 0 })
    let sq = CorralLayout.ringOffsets(sides: 4, CorralMetrics())
    #expect(Set(sq.map { ($0.x * 1000).rounded() }).count == 2)
    #expect(Set(sq.map { ($0.y * 1000).rounded() }).count == 2)
}

@Test("TP-SHAPE-04 neighbouring drops keep the configured gap", arguments: 3...8)
func gaps(sides: Int) {
    let m = CorralMetrics()
    let blobs = CorralLayout.layout(rankedIDs: ids(sides), metrics: m).blobs
    for i in 0..<sides {              // consecutive ranks are ring neighbours
        let a = blobs[i].offset, b = blobs[(i + 1) % sides].offset
        #expect(abs(hypot(a.x - b.x, a.y - b.y) - (2 * m.blobRadius + m.gap)) < 0.01)
    }
}

@Test("TP-SHAPE-05 every drop fits inside the canvas with padding", arguments: 0...8)
func canvasFits(n: Int) {
    let m = CorralMetrics()
    let (blobs, canvas, pivot, _) = CorralLayout.layout(rankedIDs: ids(n), metrics: m)
    for b in blobs {
        let c = CGPoint(x: pivot.x + b.offset.x, y: pivot.y + b.offset.y)
        #expect(c.x - b.radius >= m.padding - 0.01 && c.y - b.radius >= m.padding - 0.01)
        #expect(c.x + b.radius <= canvas.width - m.padding + 0.01)
        #expect(c.y + b.radius <= canvas.height - m.padding + 0.01)
    }
}

@Test("TP-SHAPE-06 more than 8 IDs never produce more than 8 drops")
func cappedAtEight() {
    let blobs = CorralLayout.layout(rankedIDs: ids(12)).blobs
    #expect(blobs.count == 8)
    #expect(blobs.map(\.id) == Array(ids(8)))      // the top 8 of whatever the core sent
}

@Test("TP-RANK-01 rank 1 sits in the most top-left slot", arguments: 3...8)
func rankOneTopLeft(sides: Int) {
    let blobs = CorralLayout.layout(rankedIDs: ids(sides)).blobs
    let first = blobs.first { $0.rank == 1 }!.offset
    let topLeftness = { (p: CGPoint) in p.x + p.y }            // smaller = further up-and-left
    #expect(topLeftness(first) <= blobs.map { topLeftness($0.offset) }.min()! + 0.01)
}

@Test("TP-RANK-02 ranks continue clockwise on screen", arguments: 3...8)
func clockwise(sides: Int) {
    let blobs = CorralLayout.layout(rankedIDs: ids(sides)).blobs.sorted { $0.rank < $1.rank }
    for i in 0..<sides {
        let a = blobs[i].offset, b = blobs[(i + 1) % sides].offset
        // In y-down space, a positive 2D cross product (a × b) means b is clockwise from a.
        #expect(a.x * b.y - a.y * b.x > 0)
    }
}

@Test("TP-RANK-03 dumbbell: rank 1 left, rank 2 right")
func dumbbellOrder() {
    let blobs = CorralLayout.layout(rankedIDs: ids(2)).blobs
    #expect(blobs.first { $0.rank == 1 }!.offset.x < 0)
    #expect(blobs.first { $0.rank == 2 }!.offset.x > 0)
}

@Test("TP-RANK-04 octagon tie-break: rank 1 is the left end of the top edge")
func octagonTieBreak() {
    let blobs = CorralLayout.layout(rankedIDs: ids(8)).blobs
    let r1 = blobs.first { $0.rank == 1 }!.offset
    let minY = blobs.map(\.offset.y).min()!
    #expect(abs(r1.y - minY) < 0.001 && r1.x < 0)
}

@Test("TP-RANK-05 shader payload follows ring order")
func payloadOrder() {
    let (blobs, _, pivot, _) = CorralLayout.layout(rankedIDs: ids(6))
    let (values, ring) = CorralLayout.shaderPayload(blobs.shuffled(), pivot: pivot)
    #expect(ring == 6)
    #expect(values.count == 24)
    let firstRank1 = blobs.first { $0.rank == 1 }!
    #expect(values[0] == Float(pivot.x + firstRank1.offset.x))
}

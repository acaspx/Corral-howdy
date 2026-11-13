// CorralLogoTests.swift: TP-LOGO-09..10, the Swift side of provider logos (ADR-015, docs/07 §8.1).
// Rust sends each drop's logo as a path into Corral's logos folder (Fixtures/bridge/
// roster-logos-v1.json). Swift trusts nothing: a path that isn't a logo in a logos folder is
// dropped (the drop keeps its monogram), and the file is size-checked and decoded by ImageIO as
// a bounded thumbnail.
import Testing
import Foundation
import CoreGraphics
import ImageIO
import UniformTypeIdentifiers
@testable import CorralNative

private func fixture(_ name: String) throws -> Data {
    let root = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    return try Data(contentsOf: root.appendingPathComponent("Fixtures/bridge/\(name)"))
}

private func roster(logo: String) -> Result<CorralRoster, CorralRoster.DecodeError> {
    let escaped = String(data: try! JSONEncoder().encode(logo), encoding: .utf8)!
    let json = #"{"v":1,"hidden":0,"agents":[{"id":"claude","name":"Claude","monogram":"C","status":"available","logo":\#(escaped)}]}"#
    return CorralRoster.decode(Data(json.utf8))
}

/// A fresh logos folder for one test.
private func logosFolder() throws -> URL {
    let dir = FileManager.default.temporaryDirectory
        .appendingPathComponent("corral-logo-tests-\(UUID().uuidString)")
        .appendingPathComponent("logos")
    try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
    return dir
}

/// A real PNG of `side` × `side` pixels.
private func pngData(side: Int) throws -> Data {
    let space = CGColorSpace(name: CGColorSpace.sRGB)!
    let context = CGContext(data: nil, width: side, height: side, bitsPerComponent: 8, bytesPerRow: 0,
                            space: space, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    context.setFillColor(CGColor(red: 0.8, green: 0.4, blue: 0.3, alpha: 1))
    context.fill(CGRect(x: 0, y: 0, width: side, height: side))
    let image = context.makeImage()!
    let data = NSMutableData()
    let destination = CGImageDestinationCreateWithData(data as CFMutableData, UTType.png.identifier as CFString, 1, nil)!
    CGImageDestinationAddImage(destination, image, nil)
    #expect(CGImageDestinationFinalize(destination))
    return data as Data
}

@Test("TP-LOGO-09 the roster carries each drop's logo path, as the fixture Rust sends")
func rosterCarriesLogos() throws {
    let r = try CorralRoster.decode(fixture("roster-logos-v1.json")).get()
    #expect(r.agents.map(\.id) == ["claude", "grok"])
    #expect(r.agents[0].logoPath == "/Users/you/Library/Application Support/Corral/logos/anthropic.png")
    #expect(r.agents[1].logoPath == nil)
}

@Test("TP-LOGO-09 a path that isn't a logo in a logos folder is dropped and the drop stays",
      arguments: [
        "",
        "logos/anthropic.png",                                   // relative
        "/Users/you/logos/../../etc/anthropic.png",              // climbs out
        "/Users/you/logos/./anthropic.png",
        "/Users/you/other/anthropic.png",                        // not a logos folder
        "/Users/you/logos/anthropic.svg",                        // not PNG or ICO
        "/Users/you/logos/Anthropic.png",                        // not a provider ID
        "/Users/you/logos/.png",
        "/Users/you/logos/anthropic.png/",
        "/Users/you/logos//anthropic.png",
        "/Users/you/lo\u{0}gos/anthropic.png",                   // control character
        "/Users/you/logos/anthr\u{202E}gnp.png",                 // bidi override
        "/" + String(repeating: "a/", count: 600) + "logos/anthropic.png", // too long
      ])
func badLogoPathsAreDropped(path: String) throws {
    let r = try roster(logo: path).get()
    #expect(r.agents.map(\.id) == ["claude"], "the drop stays")
    #expect(r.agents[0].logoPath == nil)
    #expect(CorralLogo.load(path: path) == nil)
}

@Test("TP-LOGO-10 a logo is decoded by ImageIO as a bounded thumbnail")
func logoLoadsBounded() throws {
    let dir = try logosFolder()
    let file = dir.appendingPathComponent("anthropic.png")
    try pngData(side: 600).write(to: file)
    #expect(CorralLogo.isValidPath(file.path))
    let image = try #require(CorralLogo.load(path: file.path))
    #expect(max(image.width, image.height) <= CorralLogo.maxPixels)
    #expect(max(image.width, image.height) > 0)
}

@Test("TP-LOGO-10 a missing, oversized or broken file is no logo, and nothing traps")
func badLogoFilesAreNil() throws {
    let dir = try logosFolder()
    #expect(CorralLogo.load(path: dir.appendingPathComponent("xai.png").path) == nil, "missing")

    let big = dir.appendingPathComponent("openai.png")
    var bytes = try pngData(side: 16)
    bytes.append(Data(count: CorralLogo.maxBytes))
    try bytes.write(to: big)
    #expect(CorralLogo.load(path: big.path) == nil, "over 1 MB")

    let broken = dir.appendingPathComponent("meta.png")
    try Data("<html>not an image</html>".utf8).write(to: broken)
    #expect(CorralLogo.load(path: broken.path) == nil, "not an image")

    let folder = dir.appendingPathComponent("anthropic.png")
    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
    #expect(CorralLogo.load(path: folder.path) == nil, "a folder, not a file")
}

@Test("TP-LOGO-10 a symlink in the logos folder is not followed")
func logoSymlinkRefused() throws {
    let dir = try logosFolder()
    let target = dir.deletingLastPathComponent().appendingPathComponent("elsewhere.png")
    try pngData(side: 32).write(to: target)
    let link = dir.appendingPathComponent("xai.png")
    try FileManager.default.createSymbolicLink(at: link, withDestinationURL: target)
    #expect(CorralLogo.load(path: link.path) == nil)
}

@Test("TP-LOGO-03 a token without a logo keeps its monogram")
func tokenDefaultsToNoLogo() {
    let token = CorralAgentToken(id: "a", name: "Alpha", monogram: "A")
    #expect(token.logoPath == nil)
}

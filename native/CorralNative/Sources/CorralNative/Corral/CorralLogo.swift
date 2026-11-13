// CorralLogo.swift
// Provider logos in the drops (ADR-015, docs/07 §8.1). Rust fetches each connected provider's
// logo from the provider's own site, checks it, saves it as `<data folder>/logos/<provider>.png`
// (or `.ico`) and sends the path on the roster. Swift trusts none of that (AGENTS.md, rules
// learned 2026-11-07): the path must name a logo in a logos folder, the file is opened without
// following links and must be a regular file of at most 1 MB, and ImageIO decodes it as a
// thumbnail of at most 256 px. Anything else is no logo, and the drop shows its monogram.

import CoreGraphics
import Foundation
import ImageIO

public enum CorralLogo {
    /// The largest logo file read.
    public static let maxBytes = 1024 * 1024
    /// The largest side of the decoded thumbnail (a drop is far smaller).
    public static let maxPixels = 256
    /// The longest path accepted, in UTF-8 bytes.
    public static let maxPathLength = 1024
    /// An ICO holds at most this many images to choose from.
    static let maxImages = 64

    /// Whether `path` is `<absolute folders>/logos/<provider id>.png|ico`, with no empty, `.` or
    /// `..` part and no control or format characters.
    public static func isValidPath(_ path: String) -> Bool {
        guard path.hasPrefix("/"), path.utf8.count <= maxPathLength,
              !path.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) })
        else { return false }
        let parts = path.split(separator: "/", omittingEmptySubsequences: false).dropFirst()
        guard parts.count >= 2,
              parts.allSatisfy({ !$0.isEmpty && $0 != "." && $0 != ".." }),
              parts[parts.endIndex - 2] == "logos",
              let name = parts.last
        else { return false }
        return isLogoName(name)
    }

    /// `<provider id>.png` or `.ico`: 1 to 32 of a–z, 0–9 and `-`.
    static func isLogoName(_ name: Substring) -> Bool {
        guard let dot = name.lastIndex(of: ".") else { return false }
        let stem = name[..<dot]
        let ext = name[name.index(after: dot)...]
        guard ext == "png" || ext == "ico", (1...32).contains(stem.count) else { return false }
        return stem.unicodeScalars.allSatisfy { ("a"..."z").contains($0) || ("0"..."9").contains($0) || $0 == "-" }
    }

    /// The logo at `path`, decoded, or nil. Never traps.
    public static func load(path: String) -> CGImage? {
        guard isValidPath(path), let data = read(path) else { return nil }
        let sourceOptions = [kCGImageSourceShouldCache: false] as CFDictionary
        guard let source = CGImageSourceCreateWithData(data as CFData, sourceOptions) else { return nil }
        let count = min(CGImageSourceGetCount(source), maxImages)
        guard count > 0 else { return nil }
        let thumbnail: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceThumbnailMaxPixelSize: maxPixels,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true,
        ]
        return CGImageSourceCreateThumbnailAtIndex(source, largest(source, count: count), thumbnail as CFDictionary)
    }

    /// Reads at most `maxBytes` from a regular file, without following a link at `path`.
    private static func read(_ path: String) -> Data? {
        let fd = open(path, O_RDONLY | O_NOFOLLOW | O_CLOEXEC | O_NONBLOCK)
        guard fd >= 0 else { return nil }
        let handle = FileHandle(fileDescriptor: fd, closeOnDealloc: true)
        var info = stat()
        guard fstat(fd, &info) == 0, (info.st_mode & S_IFMT) == S_IFREG,
              info.st_size > 0, info.st_size <= off_t(maxBytes)
        else { return nil }
        // The file could grow after fstat: read one byte past the cap to notice.
        guard let data = try? handle.read(upToCount: maxBytes + 1),
              !data.isEmpty, data.count <= maxBytes
        else { return nil }
        return data
    }

    /// The image with the most pixels (an ICO holds several sizes; a PNG has one).
    private static func largest(_ source: CGImageSource, count: Int) -> Int {
        var best = 0
        var bestArea = -1
        for index in 0..<count {
            let properties = CGImageSourceCopyPropertiesAtIndex(source, index, nil) as? [CFString: Any]
            let width = properties?[kCGImagePropertyPixelWidth] as? Int ?? 0
            let height = properties?[kCGImagePropertyPixelHeight] as? Int ?? 0
            let area = width.multipliedReportingOverflow(by: height)
            let size = area.overflow ? Int.max : area.partialValue
            if size > bestArea { best = index; bestArea = size }
        }
        return best
    }
}

/// Decoded logos by path, so a drop doesn't decode on every frame. A path that didn't decode is
/// remembered too, so a broken file isn't retried each frame.
@MainActor
final class CorralLogoCache {
    static let shared = CorralLogoCache()
    private static let capacity = 16
    private var images: [String: CGImage?] = [:]

    func image(at path: String) -> CGImage? {
        if let cached = images[path] { return cached }
        if images.count >= Self.capacity { images.removeAll() }
        let image = CorralLogo.load(path: path)
        images[path] = .some(image)
        return image
    }
}

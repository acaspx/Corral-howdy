# CorralNative

The macOS-only Swift layer of Corral: the liquid-glass corral surface, glass activity cards and checkpoints, actionable notifications, and Touch ID. Design spec: [`docs/07-brand-and-corral-surface.md`](../../docs/07-brand-and-corral-surface.md).

**Requires macOS 26 / Xcode 26.** **Status: Spike S5 prototype.** The geometry and shader maths were checked with a numpy render and a WebGL port (`preview/`). The Swift and Metal files have **not been compiled yet**. Building them on a Mac is the first task of Spike S5.

| File | What it is |
|---|---|
| `Sources/CorralNative/Corral/CorralLayout.swift` | Pure shape grammar (up to an octagon) and usage-ranked slots (rank 1 top-left, clockwise). Unit-tested |
| `Sources/CorralNative/Corral/CorralGlass.metal` | The liquid-glass SDF shader (`[[stitchable]]`, for SwiftUI `.layerEffect`) |
| `Sources/CorralNative/Corral/CorralGlassView.swift` | SwiftUI view: system glass + shader, drop tokens, polar morph springs (reorders travel around the ring), accessibility |
| `Sources/CorralNative/Corral/CorralPanel.swift` | Non-activating NSPanel at bottom center (+ wallpaper backdrop, fallback only) |
| `Tests/CorralNativeTests/CorralLayoutTests.swift` | TP-SHAPE-01..06, TP-RANK-01..05 (Swift Testing) |
| `preview/corral-shape-preview.html` | Open in a browser: same maths in WebGL, add/remove/deploy drops, simulate usage to watch reorders, 8-agent chooser. `?n=6` sets the count |
| `preview-shapes.png` | Reference render of every shape (0–8 agents) |

Build and test with Xcode or `xcodebuild` (SwiftPM's command-line build doesn't compile Metal):
```
xcodebuild test -scheme CorralNative -destination 'platform=macOS'
```

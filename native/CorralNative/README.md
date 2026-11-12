# CorralNative

The macOS-only Swift layer of Corral: the liquid-glass corral surface with agent logos, hover cards (progress and checkpoint buttons), the trace tree, actionable notifications, and Touch ID. Design spec: [`docs/07-brand-and-corral-surface.md`](../../docs/07-brand-and-corral-surface.md).

**Requires macOS 26 / Xcode 26.** **Status: Spike S5 prototype.** The package builds on macOS 26 and its tests (TP-SHAPE-*, TP-RANK-*) pass in CI. The geometry and shader maths were also checked with a numpy render and a WebGL port. `preview/corral-shape-preview.html` is the interactive design reference for logos, hover cards and the trace tree (Spike S6). Still to do in S5: tune the glass on real hardware and choose how the Metal pass and system Liquid Glass combine (docs/07 §4).

| File | What it is |
|---|---|
| `Sources/CorralNative/Corral/CorralLayout.swift` | Pure shape grammar (up to an octagon) and usage-ranked slots (rank 1 top-left, clockwise). Unit-tested |
| `Sources/CorralNative/Corral/CorralGlass.metal` | The liquid-glass SDF shader (`[[stitchable]]`, for SwiftUI `.layerEffect`) |
| `Sources/CorralNative/Corral/CorralGlassView.swift` | SwiftUI view: system glass + shader, drop tokens, polar morph springs (reorders travel around the ring), accessibility |
| `Sources/CorralNative/Corral/CorralPanel.swift` | Non-activating NSPanel at bottom center (+ wallpaper backdrop, fallback only) |
| `Sources/CorralNative/Cards/` | Each drop's card (SK-6): `CorralHoverState` (when it opens, pins and closes), `CorralCardModel` (what it shows per state), `CorralCardView` (paper), `CorralCardController` (its panel, hover sources, Esc and click-away) |
| `Sources/CorralPlayground/Playground.swift` | Desktop playground: the real corral panel at the bottom of your screen with fake agents, plus a control window that plays the Rust core's part |
| `Tests/CorralNativeTests/CorralLayoutTests.swift` | TP-SHAPE-01..06, TP-RANK-01..05 (Swift Testing) |
| `Tests/CorralNativeTests/CorralRobustnessTests.swift` | TP-SHAPE-08..11, TP-PLACE-04..05 |
| `preview/corral-shape-preview.html` | Open in a browser: same maths in WebGL, add/remove/deploy drops, simulate usage to watch reorders, 8-agent chooser. `?n=6` sets the count |
| `preview-shapes.png` | Reference render of every shape (0–8 agents) |

Build and test with Xcode or `xcodebuild` (SwiftPM's command-line build doesn't compile Metal):
```
xcodebuild test -scheme CorralNative-Package -destination 'platform=macOS'
```

## Try it on your desktop

The playground puts the real `CorralPanel` and `CorralGlassView` at the bottom center of your screen, with fake agents and no Rust core. A control window adds and removes agents, promotes the least-used one (watch it glide around the ring to top-left), shuffles, and switches between "Float above windows" and "Desktop only". Click a drop to cycle its status.

```bash
cd native/CorralNative
xcodebuild -scheme CorralPlayground -destination 'platform=macOS' -derivedDataPath .build/xcode build
.build/xcode/Build/Products/Debug/CorralPlayground --agents 5
```

Close the control window or press Command-Q to quit. What to look for is the Spike S5 checklist in [docs/03](../../docs/03-build-plan.md): does the glass refract what's behind it, do bridges and morphs look liquid, does the empty space between drops let clicks through to the windows underneath, and does an idle corral stop drawing frames (Activity Monitor › Energy).


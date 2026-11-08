# Corral: Brand and the Liquid-Glass Corral Surface

Status: Draft v0.1 · Last updated: 2026-11-07
Prototype code: `native/CorralNative/Sources/CorralNative/Corral/` · Browser preview: `native/CorralNative/preview/corral-shape-preview.html` (open it, or add `?n=5` to the URL). It includes usage ranking and the 8-agent chooser

---

## 1. The idea

The corral is a physical-feeling object on your desktop: a pool of liquid glass sitting at the bottom center of the screen, just above the Dock. Each agent is a drop of glass. Drops in the corral are linked by thin liquid bridges, and the pool's **shape is the headcount**: one drop is a circle, two make a dumbbell, three a triangle, four a square, and so on. When you deploy an agent, its drop pinches off and floats up to become its activity card, and the corral re-forms around the agents still at home. Sending it back reverses the motion and the drop rejoins the shape.

You can tell how many agents are home, which one you use most, and whether any need you, without reading a word.

Visual reference: clear, slightly blue-grey glass drops on a light grey ground, joined by narrow necks, with a soft contact shadow down-right, a bright meniscus rim, and a few tiny air bubbles inside.

## 2. Brand principles
1. **Material first, chrome never.** No borders, title bars or toolbars on the corral. The glass is the interface.
2. **Shape carries meaning.** Geometry encodes count; motion encodes state change; a small glyph plus a pulse encodes "needs you". Never color alone.
3. **Quiet at rest.** A still corral uses no animation frames. It moves only when something changes or you hover.
4. **Honest glass.** The glass bends what is actually behind it (see §4). No fake gradients pretending to be refraction.

## 3. Shape grammar

| Agents in the corral | Shape | Notes |
|---|---|---|
| 0 | Empty puddle | A small, flattened puddle, so the corral never disappears entirely |
| 1 | Circle | |
| 2 | Dumbbell | Two drops, one bridge |
| 3 | Triangle | Point up, flat bottom edge, hollow center |
| 4 | Square | Axis-aligned, hollow center |
| 5–8 | Regular polygon | Always a flat bottom edge, so the corral "sits" on the screen edge |
| More than 8 | Still an octagon | The corral never shows more than 8 drops. The user chooses which 8 (§3.2) |

Rules:
- **Count = shown agents in the corral**, meaning agents the user has chosen to show that are configured, reachable and not deployed. Deployed agents are out of the shape and on their cards.
- **Geometry:** drop radius 34 pt, edge-to-edge gap 40 pt, bridge radius 22% of drop radius, smooth-union neck 9 pt. These were tuned against the reference image in a render pass. The polygon circumradius comes from the gap: `R = (2r + gap) / (2·sin(π/n))`.
- **Size ceiling:** an octagon is about 330 pt tall. Spike S5 decides whether drops shrink for n ≥ 6 (candidate: 34 → 28 pt).
- Unavailable agents (bad key, provider down) stay in the shape but render **frosted and dim**, with an error glyph. Their state is still counted honestly.

### 3.1 Order: most used at top-left, then clockwise

- **Rank 1 (most used) sits in the top-left slot.** That's the vertex pointing most toward the top-left corner: the apex of the triangle, the top-left corner of the square, the left end of the top edge on the hexagon and octagon (for the octagon's tie between two vertices, the higher one wins). For the dumbbell, rank 1 is the left drop.
- **Ranks 2, 3 and so on follow clockwise.**
- **The order updates with usage.** The Rust core keeps a `usage_rank` projection over the event log:
  - **Score** = deploys in the last 90 days, each weighted by recency (14-day half-life), so this week's habits count more than last month's.
  - **Tie-breaks:** most recent deploy, then the order agents were added. This keeps the order stable.
  - **Recomputed** whenever a run completes or an agent is added or removed.
- **When it moves:** a new order is applied only when the corral is idle, meaning no hover, drag, open card or pending checkpoint, so a drop never slides out from under the pointer. Two neighbors swap only when the challenger's score beats the incumbent's by at least 10%, so near-ties don't flicker back and forth.
- **How it moves:** drops glide *around* the ring to their new slots (springs in polar coordinates, shortest arc). They never cut across the hollow center.
- Ranking counts every agent, including deployed and hidden ones. The drops in the corral take slots by their relative rank, so if #1 is out on a task, the next most used holds the top-left until it returns.
- **Optional setting "Keep order fixed"** (off by default) freezes the current order for people who prefer muscle memory to adaptivity.

### 3.2 Choosing which agents show (more than 8)

- The first 8 agents a user adds show automatically.
- Adding a 9th opens a **"Your corral holds up to 8"** sheet: every agent is listed by usage, the current 8 are checked, and the user picks which 8 show. The same list lives in Settings → Corral.
- Hidden agents are not second-class. They're reachable from the prompt bar (type `@` and a name) and the main window, they get activity cards when deployed, and their checkpoints still notify.
- Removing a shown agent shrinks the corral. Nothing is promoted automatically, because the user decides what's in view.
- VoiceOver announces the hidden count ("Corral, 8 agents available, most used first. 3 more not shown").

Shape and slot order are pure geometry in `CorralLayout.swift` (TP-SHAPE-*, TP-RANK-*). The ranking and the shown set are decided in the Rust core (TP-USAGE-*), which hands Swift a list of up to 8 IDs, most used first.

## 4. Rendering: how the glass is made

**Corral requires macOS 26** (ADR-011), so Apple's system Liquid Glass is always available. The glass gets real refraction of whatever is behind it, including other apps' windows, with no Screen Recording permission.

**Two layers:**
1. **Material: system Liquid Glass** (`.glassEffect()`), which supplies the real backdrop refraction, blur and adaptive tint that match the rest of macOS 26.
2. **Shape and signature details: a custom Metal shader** (`CorralGlass.metal`, a `[[stitchable]]` function used through SwiftUI's `.layerEffect`). One signed-distance field holds every drop and bridge, smooth-unioned. From that field the shader derives:
   - the **outline** the glass is carved into: drops plus thin liquid bridges, anti-aliased
   - a **height profile**: flat top, rounded bevel at the rim, like water on glass, with extra refraction along the bevel and a slight chromatic split at the edge
   - **lighting**: a specular highlight from a top-left key light, Fresnel edge tint (brand glass tint `#9EB8D1`), a bright meniscus line, and an inner shadow on the far side of the bevel
   - **detail**: a very slow caustic drift and sparse tiny air bubbles
   - **contact shadow**: soft, offset down-right, drawn by the shader (the window shadow is off)

**How the two layers combine is the main question for Spike S5.** In order of preference:

| Plan | How | Risk |
|---|---|---|
| A | Metal `.layerEffect` applied over a `.glassEffect()` layer: the shader samples the glass and carves and lights it | The system glass may not be readable from a layer effect |
| B | Each drop is a `Circle().glassEffect()` and each bridge a `Capsule().glassEffect()` inside a `GlassEffectContainer`, which merges nearby glass shapes into one liquid form natively. Metal draws only the highlights, rim and caustics as an overlay | Less control over neck shape; must check the merge looks like our bridges |
| C | Metal shader over a crop of the wallpaper image (`NSWorkspace.desktopImageURL`) | Doesn't refract windows; fallback only |

The prototype code is wired for plan A, and plan C's helper is kept in `CorralPanel.swift`. **Screen Recording is never requested** under any plan (ADR-010).

**Performance rules:**
- Render on demand: the `TimelineView` runs only while a morph spring is settling or the pointer hovers. Budget: an idle corral costs 0 frames.
- Frame budget while morphing: < 2 ms of GPU time at 2× scale on an M1.

## 5. Placement and window behavior
- A borderless, non-activating `NSPanel` (`CorralPanel.swift`). Clicking a drop never steals focus from the app you're in.
- Positioned at the **bottom center of the screen's visible frame**, so it never covers the Dock or the menu bar, wherever the user keeps them.
- **Level setting:** "Float above windows" (default, so checkpoints are always reachable) or "Desktop only" (sits on the desktop layer like a desktop object, and the summon hotkey raises it).
- Visible on all Spaces and hidden in full-screen apps. Multi-display: follows the display with the menu bar by default, and the user can pin it to a display.
- The menu-bar icon stays as a fallback entry point (settings, quit, show corral).

## 6. Interactions

| Gesture | Result |
|---|---|
| Click a drop | Opens that agent's glass card above the corral: status, last task, Deploy, Settings |
| ⌘-click drops | Multi-select (accent ring); the prompt bar offers "Deploy selected" |
| Double-click the corral background, or the summon hotkey | Prompt bar slides up from the corral (text + mic) |
| Drag a drop out of the corral | Deploys it; the prompt bar opens attached to that agent |
| Drag an activity card onto the corral | Sends that agent back to the corral |
| Recall-all hotkey | Every deployed drop flies home; the shape re-forms |

## 7. Motion

| Event | Motion |
|---|---|
| Agent deployed | Bridges to that drop thin to nothing (`attach` 1 → 0), the drop rises and becomes its activity card, the remaining drops spring to the new polygon |
| Agent returns | Reverse: the card condenses into a drop, falls into place, bridges grow back |
| Agent added | Drop grows from a bead at the shape's center |
| Needs you | The drop's rim pulses slowly (1.2 s) and a hand glyph appears |
| Shape change | Spring, response ≈ 0.45 s, light damping, so the liquid overshoots a little |
| Usage reorder | Drops glide around the ring to their new slots, shortest arc, only while the corral is idle |

**Reduce Motion:** positions crossfade with no springs, and the caustic drift is frozen. **Reduce Transparency:** same geometry, opaque frosted fill. **Increase Contrast:** stronger rim and tint.

## 8. Activity cards and checkpoints (native glass)
Deployed agents show as glass cards that fan out above the corral, newest nearest. The cards and checkpoint prompts are **SwiftUI** on system Liquid Glass with the brand tint, so everything you see at a glance shares one material. Deep-work surfaces (trace viewer, agent and policy editors, spend, settings) stay in the React main window. All state still comes from the Rust core: the Swift layer only displays it and sends intents back.

## 9. Visual tokens (starting point)

| Token | Value |
|---|---|
| `glass.tint` | `#9EB8D1` (RGB 0.62, 0.72, 0.82) |
| `glass.refraction` | 14 pt |
| `glass.bevel` | 12 pt |
| `glass.frost` | 0.35 |
| `glass.neck` | 9 pt |
| `drop.radius` | 34 pt |
| `drop.gap` | 40 pt |
| `bridge.ratio` | 0.22 |
| `shadow` | offset (3, 5) pt, falloff 14 pt, 16% |
| Type | SF Pro Rounded semibold for monograms; SF Pro for UI |
| Status glyphs | SF Symbols: `hand.raised.fill` (needs you), `checkmark` (done), `exclamationmark.triangle.fill` (error) |

These live in `CorralMetrics` and the shader parameters. Spike S5 turns them into a shared token file used by both Swift and React.

## 10. Accessibility
- The corral is one accessibility group ("Corral, 4 agents available, most used first"). Each drop is a button named "<Agent>, <status>, number <rank> by use". VoiceOver order follows rank, so it matches the visual clockwise order.
- Full keyboard path: summon hotkey → arrow keys move between drops → Return opens → ⌘-Return deploys.
- The usage reorder never moves focus: a focused drop keeps VoiceOver focus as it travels.
- Status is never color-only: needs-you adds a glyph and a pulse, and errors add a glyph.
- Contrast of monograms on glass is checked over a light, a dark and a busy test wallpaper (TP-GLASS-03).

## 11. Decisions and open questions

Decided (2026-11-07):
- Minimum macOS 26, so system Liquid Glass is always available (ADR-011).
- The corral tops out at an octagon. With more than 8 agents, the user chooses which ones show.
- Most-used agent at top-left, then clockwise. The order follows usage (ADR-012).

Open:
1. Do drops shrink for n ≥ 6, or does the corral just get bigger?
2. Agent identity in the drop: monogram, provider logo, or a user-chosen SF Symbol/emoji? Provider logos raise trademark questions, so the recommendation is monogram or symbol.
3. Should the empty puddle hide after a while, or always show?
4. Is the 14-day half-life right? Validate it with dogfood usage in M8.

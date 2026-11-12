# Corral: Brand and the Liquid-Glass Corral Surface

Status: Draft v0.1 · Last updated: 2026-11-09
Prototype code: `native/CorralNative/Sources/CorralNative/Corral/` · Interactive design preview: `native/CorralNative/preview/corral-shape-preview.html` (open it; `?n=6` sets the agent count, `?hover=0` opens the first card). It shows logos, hover cards with buttons, the trace tree, usage ranking and the 8-agent chooser

---

## 1. The idea

The corral is a physical-feeling object on your desktop: a pool of liquid glass sitting at the bottom center of the screen, just above the Dock. Each agent is a drop of glass carrying **that agent's logo**. Drops are linked by thin liquid bridges, and the pool's **shape is the headcount**: one drop is a circle, two make a dumbbell, three a triangle, four a square, and so on up to an octagon.

**Agents stay in the corral while they work.** A working drop wears a slowly turning ring, and a drop that needs you pulses and shows a badge. Above the corral, a **trace tree** rises: one column per active task. Each column is the prompt you gave, the task card, and a stack of subtask cards, wired down into the agent's drop. **Hover any drop** and a paper card opens above it, showing what the agent is doing, how far along it is, and the buttons for anything waiting on you: Approve, Deny, Edit.

You can tell how many agents you have, which one you use most, which are busy and which need you, without reading a word.

Visual references (owner-provided screenshots, not stored in the repo): clear, slightly blue-grey glass drops joined by narrow necks; Devin's session visuals (https://devin.ai), meaning white cards wired together by hairline connector branches, dark glass prompt pills and parameter chips; and a warm "paper" card style with a double rim, serif headline and small-caps label.

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
- **Count = shown agents.** Working agents stay in the shape; their state shows on the drop (ring, badge) and in the trace tree (§8.3).
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
- Ranking counts every agent, including hidden ones. Shown drops take slots by their relative rank. Starting a task doesn't move a drop, but finishing one can change the order the next time the corral is idle.
- **Optional setting "Keep order fixed"** (off by default) freezes the current order for people who prefer muscle memory to adaptivity.

### 3.2 Choosing which agents show (more than 8)

- The first 8 agents a user adds show automatically.
- Adding a 9th opens a **"Your corral holds up to 8"** sheet: every agent is listed by usage, the current 8 are checked, and the user picks which 8 show. The same list lives in Settings → Corral.
- Hidden agents are not second-class. They're reachable from the prompt bar (type `@` and a name) and the main window, when one is working it still gets a trace column (its wire ends at a small "hidden" bead at the corral's edge, and its card opens from the column), and their checkpoints still notify.
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
- **Level setting** (decided 2026-11-12), from the corral's right-click menu and remembered across launches: **"On the desktop"** (the default: the desktop layer, behind app windows, like a desktop object; Show Desktop reveals it, and from M4 the summon hotkey raises it), **"Like other windows"** (in front while Corral is active, behind other apps after you switch), or **"Always on top"**. Checkpoints still reach the user on any level: they raise an actionable notification, and the card floats above app windows. Settings → Corral offers the same choice from M4.
- Visible on all Spaces and hidden in full-screen apps. Multi-display: follows the display with the menu bar by default, and the user can pin it to a display.
- The menu-bar icon stays as a fallback entry point (settings, quit, show corral).

## 6. Interactions

| Gesture | Result |
|---|---|
| Hover a drop (120 ms intent delay) | Its hover card opens above it (§8.2). It stays open while the pointer is on the drop or the card, and closes 260 ms after leaving |
| Click a drop, or focus it and press Space | Pins the hover card. For an idle agent the card opens on **New task** |
| ⌘-click drops | Multi-select (accent ring); the prompt bar offers "Deploy selected" |
| Double-click the corral background, or the summon hotkey | Prompt bar appears above the corral (text + mic), with no animation (§7) |
| Drag a drop upward | Shortcut for "New task": the prompt bar opens attached to that agent. The drop stays in the corral |
| Click a task card in the trace tree | Opens that run in the trace viewer (main window) |
| Click a subtask stack | Fans the stack out into a list of every subtask with its status |
| Recall-all hotkey | Every working agent stops; trace columns fold back down into their drops |

## 7. Motion

The corral is glanced at all day, so motion has to earn its place: it shows a **state change** or keeps **spatial continuity** (where did that drop go?). Anything you trigger many times a day, or from the keyboard, doesn't animate. Values follow the motion standards in [`.claude/skills/review-animations/STANDARDS.md`](../.claude/skills/review-animations/STANDARDS.md): UI stays under 300 ms, enters and exits use a strong ease-out, nothing grows from nothing, and only the liquid uses springs.

### 7.1 The liquid (springs)
| Moment | Motion | Reduce Motion |
|---|---|---|
| Shape change (agent added or removed) | Drops spring to the new polygon: `motion.liquid`, polar paths so drops travel around the ring, never across the hollow center | Positions snap; joining and leaving drops fade (`motion.fade`) |
| Agent added | The drop grows from a bead (20% size) between the ring and the pivot and attaches to its neighbors; its logo fades in over the last 40% of the growth | Fades in at full size |
| Agent removed or hidden | The drop detaches and **pinches off in place**: no travel. Its logo fades out first (`motion.exit`), then the glass shrinks away | Fades out |
| Usage reorder | Drops glide around the ring along the shortest arc (`motion.liquid`), only while the corral is idle (§3.1) | Snaps, while idle |
| Press a drop | Scales to 0.96 (`motion.press`), back on release. No motion on hover: hover opens a card instead | No scale |

### 7.2 Cards and status (curves, under 300 ms)
| Moment | Motion | Reduce Motion |
|---|---|---|
| Hover card opens | After the 120 ms intent delay: opacity 0→1, scale 0.97→1 and 8 pt rise, anchored at its bottom edge over the drop (`motion.enter`). The other trace columns fade to 28% | Opacity only |
| Hover card closes | Opacity only (`motion.exit`), after the 260 ms grace | Same |
| Moving to another drop with a card open | No second intent delay. The card **retargets** to the new drop with a short spring (`motion.retarget`) and its content crossfades (`motion.swap`) | Content swaps; card jumps |
| Card content changes state (working → needs you → done) | Crossfade with a 2 pt blur so the two states read as one change (`motion.swap`) | Crossfade without blur |
| Task starts | The trace column builds up from the drop: wire draws upward (trim 0→1), then prompt pill, task card and subtask stack enter in that order, 50 ms apart, each rising 8 pt (`motion.enter`). The working ring fades in (`motion.fade`) | Column fades in as one |
| Subtask advances | The finished card fades and lifts 6 pt (`motion.exit`); the next card rises from its peek position (`motion.retarget`). Interruptible: a fast run of steps retargets, it never queues | Text swaps in place |
| Done, Send back, Recall-all | The column folds down into its drop: cards drop 8 pt and fade, top first, 30 ms apart (`motion.exit`); the drop settles from 0.97 to 1 as they land | Column fades out |
| Done or error badge | Pops in from 0.9 with opacity (`motion.enter`) | Fades in |

### 7.3 Continuous motion (the only motion that runs on its own)
| Moment | Motion | Reduce Motion |
|---|---|---|
| Working | A coral arc turns at a constant speed (`motion.spin`, linear). It runs as a Core Animation rotation on its own layer, so the glass shader doesn't redraw every frame | Static arc showing plan progress (TP-LOGO-04) |
| Needs you | The ring turns maroon and pulses (scale 1→1.06 and back, `motion.pulse`) **three times**, then stays steady with the "!" badge. It pulses again only when a new checkpoint opens: attention without nagging | No pulse; badge and steady maroon ring |
| Caustics | Drift only while something else is moving; frozen at rest so an idle corral draws nothing (§2, principle 3) | Frozen |

### 7.4 Never animated
- **The prompt bar.** It's summoned by hotkey many times a day, so it appears and disappears instantly, like a command palette
- **Typing, the transcript, and selection** (⌘-click rings appear instantly)
- **Numbers that tick:** elapsed time, spend, step counts. They change in place in tabular figures, so they don't jitter
- **Approve, Deny and Edit buttons.** They ignore input for 300 ms after a card appears (§8.2); they don't fade or move during that time, so their position is never in doubt
- **Anything in the approval box.** It's the one place you must read exactly; it never moves while you decide

**Reduce Transparency:** same geometry, opaque frosted fill; translucent cards become paper. **Increase Contrast:** stronger rim, tint and wire color.

## 8. Logos, hover cards and the trace tree

All of these are **SwiftUI** in CorralNative. As everywhere, state comes from the Rust core; Swift only displays it and sends intents (approve, deny, new task) back. Deep-work surfaces (trace viewer, editors, spend, settings) stay in the React main window.

### 8.1 Agent logos in the drops
- Every agent has an **icon**: the logo the user assigns when adding the agent (PNG, PDF or SVG). Corral suggests a neutral provider glyph until one is set, and falls back to a monogram (SF Pro Rounded) if there's neither.
- **The repo ships no third-party logos.** Provider marks are trademarks with their own brand guidelines, so users bring their own. A curated "provider marks" pack is possible later, only after checking each provider's guidelines.
- **Rendering:** centered in the drop at 46% of its diameter, drawn above the glass (not refracted), full color. Monochrome logos can opt into template mode so they adapt to light and dark.
- **Status around the logo:**
  - working: a coral arc turning slowly
  - needs you: a maroon pulsing ring plus a "!" badge
  - done: a check badge
  - error: a triangle badge
  - Status never relies on color alone.
- **Imports are untrusted input** (security model T19): copied into Corral's own folder and never hot-linked, limited to 1 MB and 1024 px, decoded with ImageIO, and SVGs rasterized once at import (no scripts, no external references).

### 8.2 Hover card (paper)
Opens above the hovered drop, centered and clamped to the screen edges, never covering the drop itself.

**How it opens and closes** (decided 2026-11-12, the pattern of consumer agent apps; TP-HOVER-01):
- **Hover** a drop: its card appears after 120 ms, in its own non-activating panel, so the app in front keeps focus. It stays while the pointer is on the drop or the card and closes 260 ms after the pointer leaves both.
- **Click** a drop: the card is **pinned**. Its panel takes keyboard focus, so the cursor goes straight into the task field, without bringing Corral's other windows forward. While a card is pinned, hovering other drops doesn't swap it; clicking another drop pins that one instead.
- **Close** a pinned card with **×** (top right), **Esc**, or a click anywhere else.
- **The task field** sits at the bottom of the card (Return sends): "Give Scout a task" for an agent in the corral, "Ask a follow-up" on a done card (same session), "Give Scout a new task" after Stop. A working card shows Stop instead, and an error card offers Retry. The general prompt bar (M4) is separate: it slides up above the corral for `@agent`, voice and Deploy all.

Contents, top to bottom:
1. **Label:** `AGENT · PROVIDER` in maroon small caps, with a chevron that opens the trace
2. **Headline:** the task title in New York serif, or "Ready for a task"
3. **Status line:** "Step 4 of 5 · 1m 16s", or "Needs your approval · step 5 of 5"
4. **Progress:** one segment per plan step (done / now / skipped), then "Now: <step> · elapsed · spend"
5. **Approval box**, when a checkpoint is pending: "NEEDS YOUR APPROVAL", the action title, the *exact* detail (destination, command, path, verbatim), and chips for the action class and risk tier. More than one pending shows "1 of 3" with arrows
6. **Buttons**, pinned to the bottom, by state:

| State | Buttons |
|---|---|
| Idle | **New task** · Settings |
| Working | Pause · Stop · Trace › |
| Needs you | **Approve** · Deny · Edit… · Trace › |
| Done | **Send back** · New task · Trace › |
| Error | **Retry** · Send back · Trace › |

**Safety rules for the buttons** (security model T18; the policy engine still decides, this is presentation):
- Hovering never approves anything. Approve needs a click or Return on a focused button.
- Buttons ignore input for 300 ms after a card appears, so a card popping up under the pointer can't be clicked through by accident.
- High-risk actions approve with a click, but only on a **pinned** card (clicked or opened from the keyboard) and only after any truncated detail has been expanded. Critical ones read "Approve with Touch ID".
- The approval box is drawn only from the core's checkpoint data. Agent-written text (titles, plan steps, summaries) uses its own style, is length-capped, and has control and bidi characters stripped; destinations show internationalized domains as punycode (security model T20).
- Voice can't press Approve on high or critical actions.
- Approve uses the checkpoint's single-use nonce.
- Deny optionally takes a note that is sent to the agent.

### 8.3 Trace tree (translucent stacked cards)
- **Where:** centered above the corral, rising from it. One **column per active task**, at most 3 side by side; more become a "+N more running" chip. Ordered needs-you first, then most recent. When nothing is running there is no tree, so the corral stays calm.
- **Column, top to bottom:**
  - prompt pill: dark glass, the original prompt in quotes, at most 2 lines
  - a hairline stem
  - task card (translucent): agent logo, provider label, bold title, short status
  - a stem
  - subtask stack: the top card is the current subtask ("Subtask 4 of 5", title, status); up to 2 more cards peek out below it for what's left
  - a **connector wire** into that agent's drop: a 1 pt hairline with 12 pt rounded elbows, like the branch lines in the reference, turning maroon while waiting on you
- **Where subtasks come from:** the run's plan steps when the agent publishes one (ACP plan updates, a native agent's plan tool), otherwise its top-level trace spans (`invoke_agent` / tool spans). The core turns either into a `plan_view` projection, so Swift never parses provider output.

### 8.4 Card materials
| Material | Used for | Spec |
|---|---|---|
| **Glass** | Drops, prompt pill (dark tint), prompt bar, mic button | System Liquid Glass + the Metal pass (§4) |
| **Veil** (translucent card) | Trace task and subtask cards | White at 74% over an 18 pt backdrop blur, radius 16, 1 px hairline at 5% black, soft shadow; stacked cards peek 6 pt apart, each 8% narrower |
| **Paper** | Hover cards, and anything with buttons | Warm paper `#FBFAF8`, radius 23, inside a 7 pt translucent rim (radius 30): a double edge. New York serif headline, maroon small-caps label, coral data accent |
| **Chip** | Parameters in cards (action class, paths, schedules) | Light blue `#EAF1FE` with `#3B6FD8` text, radius 6; risk chips are maroon-tinted |

### 8.5 Checkpoints elsewhere
Checkpoints also raise an actionable macOS notification (§9 of the system design). High and critical risks open the hover card instead of offering inline approve in the notification.

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
| `paper.bg` | `#FBFAF8`, radius 23, rim 7 pt at 50% white (radius 30) |
| `veil.bg` | white 74%, blur 18 pt, radius 16, hairline 5% black |
| `ink` / `ink.2` / `ink.3` | `#1D1A17` / `#6F6962` / `#9B958E` |
| `accent.maroon` | `#7B2324` (labels, needs-you) |
| `accent.coral` | `#C9524A` (progress, data, working ring) |
| `chip` | `#EAF1FE` bg, `#3B6FD8` text |
| `wire` | 1 pt, `rgba(120,128,140,.45)`, 12 pt elbows |
| Type | New York (serif) for card headlines; SF Pro for UI; SF Pro Rounded for fallback monograms |
| Logo | 46% of drop diameter, above the glass |
| Status glyphs | "!" badge (needs you), `checkmark` (done), `exclamationmark.triangle.fill` (error) |
| `motion.liquid` | Spring, response 0.45 s, damping ratio 0.87 (about 0.1 bounce): stiffness 190, damping 24 at mass 1 |
| `motion.retarget` | Spring, response 0.25 s, no bounce |
| `motion.enter` | 180 ms, ease-out `cubic-bezier(0.23, 1, 0.32, 1)` |
| `motion.exit` | 120 ms, same ease-out |
| `motion.swap` | 160 ms crossfade, 2 pt blur at the midpoint |
| `motion.press` | 120 ms in, 160 ms out, ease-out, scale 0.96 |
| `motion.fade` | 150 ms opacity, ease-out (Reduce Motion's stand-in for movement) |
| `motion.spin` | One turn per 2.4 s, linear |
| `motion.pulse` | 1.2 s per pulse, ease-in-out, 3 pulses |

These live in `CorralMetrics` and the shader parameters. Spike S5 turns them into a shared token file used by both Swift and React.

## 10. Accessibility
- The corral is one accessibility group ("Corral, 4 agents, most used first, 1 needs you"). Each drop is a button named "<Agent>, <status>, number <rank> by use". VoiceOver order follows rank, so it matches the visual clockwise order.
- Hover is never the only way in: focusing a drop and pressing Space opens the same card, pinned. The card is announced as a dialog, and focus moves to its first button (Approve, when something is waiting).
- The trace tree is a list of runs; each column reads "<Agent>: <task>, subtask n of m, <status>".
- Full keyboard path: summon hotkey → arrow keys move between drops → Space opens the card → Tab through its buttons → Esc closes.
- The usage reorder never moves focus: a focused drop keeps VoiceOver focus as it travels.
- Status is never color-only: needs-you adds a glyph and a pulse, and errors add a glyph.
- Contrast of logos, fallback monograms and status rings on glass is checked over a light, a dark and a busy test wallpaper (TP-GLASS-03, TP-LOGO-03).

## 11. Decisions and open questions

Decided (2026-11-07):
- Minimum macOS 26, so system Liquid Glass is always available (ADR-011).
- The corral tops out at an octagon. With more than 8 agents, the user chooses which ones show.
- Most-used agent at top-left, then clockwise. The order follows usage (ADR-012).
- Each drop shows the agent's logo (user-assigned; no third-party marks in the repo).
- Agents stay in the corral while working; a hover card shows progress and the action buttons; a trace tree of translucent stacked cards rises above the corral (ADR-014).

Open:
1. Do drops shrink for n ≥ 6, or does the corral just get bigger?
2. Should Corral offer a curated provider-marks pack, after checking each provider's brand guidelines?
3. Should the empty puddle hide after a while, or always show?
4. Is the 14-day half-life right? Validate it with dogfood usage in M8.
5. Trace tree height: with 3 columns and long prompts, does it fit above the corral on a 13" display? Spike S6 checks; fallback is collapsing the prompt pill into the task card.

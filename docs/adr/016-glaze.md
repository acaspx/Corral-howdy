# ADR-016: The main window is built with Glaze

Status: **Accepted** 2026-11-25 (owner decision). Amends the main window's styling in ADR-001;
the corral and its drops stay native (docs/07).

## Context
The main window (API keys, agents, policies, the corral's settings, traces) was React with one
hand-written stylesheet: system colours (`Canvas`, `CanvasText`), a glass tint borrowed from the
corral, and a few dozen class rules. Each new screen added its own rules, and the controls were
plain HTML elements styled one by one. The owner chose Glaze, by design.how, for the main window:
"compact black and white for AI products, with glass only on the one action you press next". It
is a set of React components on Base UI, styled with Tailwind v4 and design tokens, with a skill
that tells a coding agent its rules and a lint that enforces them.

## Decision
The main window's UI is built only from Glaze's components and tokens. Everything except the
corral, its drops and the other native surfaces in `native/CorralNative`, which stay Swift.

- **The code** is copied into the repo (`apps/desktop/src/components/glaze/`,
  `apps/desktop/src/lib/glaze/`), the way `npx shadcn@latest add https://design.how/r/glaze/…`
  installs it, from `arla6ka/design.how` (MIT) at a pinned commit. design.how itself isn't
  reachable from Corral's build machines; the files are the same. The README there lists the
  source commit and every local change.
- **New dependencies:** `@base-ui/react`, `class-variance-authority`, `clsx`, `tailwind-merge`
  and `@phosphor-icons/react` at runtime; `tailwindcss` and `@tailwindcss/postcss` to build;
  `@typescript-eslint/parser` for Glaze's lint. All exact versions.
- **One scope:** the window's root is `.glaze` with Glaze's portal provider, so menus and dialogs
  keep the tokens. Glaze doesn't follow the system's light or dark setting by itself; the root
  does, as the window always has (TP-GLZ-01).
- **The rules:** the Glaze skill (`.claude/skills/glaze/SKILL.md`) is followed for every change
  to the main window. Glaze's lint (`glaze.eslint.config.mjs`) runs in `pnpm lint` and must report
  nothing. design.how's `migrate-design-system` and `ui-review` skills are installed for ports and
  reviews.
- **Accessibility first:** where a Glaze rule and an accessibility need meet, accessibility wins,
  as Glaze's own priority order says. Section headings stay real headings, a select stays a real
  `<select>` where the native control serves (`NativeSelect`), and every story still passes axe.

## Consequences
- The window looks and behaves as one system: pills for one-line controls, one primary action
  per group, colour only for status, no shadows, the 2px ink focus ring.
- Screens are ported one at a time, each in its own commit, in one change, so `main` never
  shows a window half on the old styles.
- Glaze's code is ours to keep up to date: a new component is copied the same way, and an update
  is a new copy from a newer commit, with the local changes reapplied.
- The corral's look (docs/07) is unchanged; the main window no longer borrows its glass tint.

## Alternatives
- **Keep the hand-written stylesheet.** Cheapest now, but every screen drifts on its own.
- **A different component kit** (shadcn's own, Radix Themes). Not what the owner chose; Glaze's
  AI components (tool calls, activity, autonomy rules) match Corral's screens closely.

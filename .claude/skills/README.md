# Agent skills

Claude Code loads skills from this folder automatically. They come from two public, MIT-licensed sources, copied unchanged.

## Emil Kowalski's skills

These skills are by **[Emil Kowalski](https://github.com/emilkowalski)**, copied unchanged from his public repository [emilkowalski/skills](https://github.com/emilkowalski/skills) ("Skills for Designers and Engineers"). His license is in [`LICENSE`](LICENSE) and applies to his skills: every folder below except the design.how ones.

- Source: `emilkowalski/skills` at commit `5206a95` (2026-11-08)
- Emil's site: [emilkowal.ski](https://emilkowal.ski) · Newsletter and course: [aiforui.dev](https://aiforui.dev/skills)

They help agents get the details of interface work right: animation easing and timing, gestures, Apple platform conventions, Swift, and reviewing UI changes. In Corral they matter most for the native surfaces in `native/CorralNative` (see [docs/07](../../docs/07-brand-and-corral-surface.md)):

| Skill | Useful for |
|---|---|
| `apple-design`, `write-swift` | The corral panel, hover cards and trace tree in SwiftUI |
| `animate`, `choreograph`, `review-animations` | Morph springs, card motion, Reduce Motion fallbacks |
| `gestures` | Hover intent, drag-to-task, pinning cards |
| `animation-performance` | The idle-corral and per-frame GPU budgets |
| `emil-design-eng`, `break-ui` | Design-engineering review of a UI change |

### Updating Emil's skills

Copy the upstream `skills/` folder over this one without editing the files, then update the commit above:

```bash
git clone --depth 1 https://github.com/emilkowalski/skills /tmp/emil-skills
cp -R /tmp/emil-skills/skills/. .claude/skills/
cp /tmp/emil-skills/LICENSE .claude/skills/LICENSE
```

Don't edit these files here. Corral-specific skills go in their own folders with their own names, so updates from upstream stay a clean copy.

## design.how's skills and Glaze

These are by **Arlan Marat** ([design.how](https://design.how)), from his public repository [arla6ka/design.how](https://github.com/arla6ka/design.how). His license is in [`LICENSE-design.how`](LICENSE-design.how) and applies to these folders.

- `glaze`: the rules of Glaze, the main window's design system (ADR-016). Follow it for every change to the main window's UI. Its code is in `apps/desktop/src/components/glaze/`. The same file is in `.agents/skills/glaze/` for other agents.
- `migrate-design-system`, `ui-review`, `build-design-system`, `token-mapping`, `component-docs`, `design-system-boss`, `make-font`, `make-logo`, `make-motion`: installed with `npx skills add arla6ka/design.how`, pinned in [`skills-lock.json`](../../skills-lock.json).
- Source: `arla6ka/design.how` at commit `1ce5304` (2026-11-25)

### Updating design.how's skills

```bash
npx skills update -p
cp <design.how checkout>/systems/glaze/SKILL.md .claude/skills/glaze/SKILL.md
cp <design.how checkout>/systems/glaze/SKILL.md .agents/skills/glaze/SKILL.md
```

## Not in the repo

Skills from paid courses (for example Emil's AI for UI course, installed with a personal token as `emil-*` folders) are licensed to the person who bought them. They're ignored by `.gitignore` and stay on your machine.

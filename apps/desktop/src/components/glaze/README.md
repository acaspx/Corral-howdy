# Glaze

The main window's design system (ADR-016): Glaze by design.how, MIT licensed (see `LICENSE`).

Copied from [`arla6ka/design.how`](https://github.com/arla6ka/design.how) at commit
`1ce5304466db2ba47a7a7524603e86df77c702bd` (`systems/glaze`), the way
`npx shadcn@latest add https://design.how/r/glaze/<name>.json` installs it: components here
and in `ui/` and `ai/`, support code in `src/lib/glaze/`, imports rewritten to the `@/` aliases
in `components.json`. design.how itself can't be reached from Corral's build machines, so the
registry wasn't used; the files are the same.

Add a component the same way: copy it and the files it imports, keep it as it is, and list any
change below.

## Local changes

- `styles.css`: `@source "../..";` added, so Tailwind scans the app's own screens. Glaze's
  `@source` paths assume `components/` at the project root; here it's under `src/`.
- `ui/field.tsx`, `ui/input.tsx`, `ui/kbd.tsx`, `src/lib/glaze/portal.ts`: an array element read
  after its length is checked uses `?.`, for TypeScript's `noUncheckedIndexedAccess`, which this
  project turns on. Behaviour is unchanged.

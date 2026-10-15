# ADR 0002: IBM Carbon replaces Geist for the Platform Shell

- Status: accepted
- Date: 2026-10-14
- Supersedes: `docs/spec/hand-off.md` §8.1 (Geist tokens and recipes)

## Context

The Tauri shell's vanilla-JS frontend used hand-written controls over
Geist-inspired tokens. Many controls were bespoke (dialogs, tables, menus),
inconsistent in keyboard and focus behaviour, and the settings screen only
exposed part of what the Core can do. The product owner asked for IBM's
Carbon Design System and for every control to be wired to a real action.

## Decision

1. The frontend is a Vite + React + TypeScript app (`packages/tauri-shell/frontend`)
   built on `@carbon/react`, `@carbon/styles` and `@carbon/icons-react`.
2. Carbon **g10** is the light theme and **g100** the dark theme. The theme is
   stored in `localStorage` and seeded from `prefers-color-scheme`.
3. IBM Plex Sans and Mono are bundled from `@ibm/plex-*` and emitted into
   `dist/assets`; nothing is fetched from a CDN, so the app works offline and
   under a strict CSP (`default-src 'self'`; `style-src` also allows inline
   styles because Carbon components set some).
4. `packages/shell-strings/en.json` stays the single string table and is
   imported through the `@strings` alias. A test fails when a `t("key")` used in
   source is missing from the table.
5. The Tauri IPC surface is wrapped once in `src/tauri/api.ts`; components never
   call `invoke` directly. Core events are pushed as `core-event`.
6. The Geist XAML theme, Geist fonts and the vanilla modules are deleted.

## Consequences

- Positive: accessible modals, tables, menus and focus handling come from one
  library; the probe sheet, queue and settings share components.
- Negative: the CSS bundle is about 1.2 MB (117 KB gzipped) because the whole
  Carbon stylesheet is included; acceptable for a desktop app. Build needs
  Node 22 and `npm ci`. The xbps template therefore ships a prebuilt
  `frontend/dist` in its source tarball.

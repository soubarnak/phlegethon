# Research: Geist design system on WinUI 3 and Tauri

**Issue:** [#5](https://github.com/soubarnak/phlegethon/issues/5)  
**Question:** How can Vercel’s Geist design system (tokens, typography, density, components) inform a clean, low-clutter UI on both WinUI 3 XAML and a Tauri web view without maintaining two unrelated visual languages?  
**Scope:** Spec research only — no full app UI.  
**Primary sources only** (no Google-owned docs).

## Verdict

Treat Geist as a **token + recipe** system, not as a shared component library. Keep one machine-readable token source of truth (Vercel’s published `design.md` / `design.dark.md`), generate **CSS custom properties** for the Tauri webview and **XAML `ResourceDictionary.ThemeDictionaries`** for WinUI 3, and reimplement chrome with each platform’s native controls styled to those tokens. Do **not** port `@vercel/geistcn` React components into XAML, and do **not** let WinUI’s default Fluent chrome diverge from the Tauri CSS skin by hand-tuning each side independently.

## What Geist actually ships

Vercel documents Geist as foundations (colors, typography, materials, grid) plus React building blocks (`@vercel/geistcn`) and assets ([Geist introduction](https://vercel.com/geist/introduction)).

| Layer | What it is | Share across shells? |
| --- | --- | --- |
| Colors | 10-step scales (`100`–`1000`) with fixed semantic intent; `background-100/200`; `gray-alpha-*`; accent scales | **Yes — shared tokens** |
| Typography | Geist Sans / Mono + named styles (`label-14`, `copy-14`, …) | **Yes — shared metrics + font files** |
| Materials / density | Radii (`sm` 6px, `md` 12px, `lg` 16px), spacing scale (base 4px), surface/shadow presets | **Yes — shared numbers** |
| React components | `@vercel/geistcn` (Button, Modal, …) | **No — web-only recipes** |
| Icons / brand | `@vercel/geistcn-assets` | Optional; prefer a small shared SVG set if needed |

Machine-readable Light/Dark specs live at:

- https://vercel.com/design.md (Light; `version: alpha`)
- https://vercel.com/design.dark.md (Dark; same token *names*, different values)

Color step intent (from [Colors](https://vercel.com/geist/colors) and `design.md`):

| Step | Intent |
| --- | --- |
| 100 / 200 / 300 | Default / hover / active background |
| 400 / 500 / 600 | Default / hover / active border |
| 700 / 800 | High-contrast fill / hover fill |
| 900 / 1000 | Secondary / primary text & icons |

`design.md` states the aesthetic explicitly: minimal, high-contrast, restrained color, color for **state and hierarchy**, not decoration — aligned with Phlegethon’s “Geist-inspired, minimal visual clutter, clear progress” charting note.

### Fonts

Geist Sans, Mono (and Pixel) are published from [vercel/geist-font](https://github.com/vercel/geist-font) under the **SIL Open Font License 1.1** (`OFL.txt` / `LICENSE.txt` in that repo). OFL allows bundling and embedding with software. Prefer the GitHub release zip (e.g. `geist-font-v1.7.2.zip`) or the `geist` npm package’s font files — **not** Google Fonts mirrors — so packaging stays on Vercel-owned sources.

### App-relevant type subset (from `design.md`)

Marketing sizes (72–40) are out of scope for a download-manager shell. Use:

| Token | Size / weight / line-height | Typical use in Phlegethon |
| --- | --- | --- |
| `heading-20` / `heading-16` | 20/16px, weight 600 | Window / section titles |
| `label-14` | 14/400/20 | Primary UI labels, queue row title |
| `label-13` / `label-12` | 13–12px | Secondary lines, meta |
| `label-13-mono` / `label-14-mono` | Mono | Speeds, bytes, ETA (tabular feel) |
| `copy-14` / `copy-13` | Multi-line | Empty states, confirm copy |
| `button-14` / `button-12` | 14/12, weight 500 | Buttons |

### Density (from `design.md`)

- **Spacing:** `base` = 4px; steps `1`→4px … `4`→16px, `6`→24px, `8`→32px (queue row padding typically `spacing.2`–`spacing.3`).
- **Radius:** `sm` 6px (buttons/inputs — matches materials “everyday” 6px), `md` 12px (menus/cards), `lg` 16px (fullscreen chrome).
- **Control height:** primary button/input **40px** (`components.button-primary` / `input` in `design.md`).

## Shared token source of truth

### Recommendation

1. **Canonical files in-repo:** vendor snapshots of `design.md` and `design.dark.md` (or a JSON export generated from them) under something like `design/geist/` with a pinned fetch date and upstream URL. Treat upstream as `version: alpha` — expect churn.
2. **Generate two consumers from that single tree:**
   - `tokens.css` — CSS variables for Tauri (`--ds-background-100`, `--ds-gray-1000`, `--space-4`, `--radius-sm`, …).
   - `GeistTheme.xaml` — `ResourceDictionary` with `ThemeDictionaries` keyed `Light`, `Dark`, and a careful `HighContrast` policy (see below).
3. **Do not** maintain separate Figma-only or hand-copied hex tables per platform.

Illustrative mapping (values from published specs):

| Token | Light | Dark |
| --- | --- | --- |
| `background-100` | `#ffffff` | `#000000` |
| `gray-100` | `#f2f2f2` | `#1a1a1a` |
| `gray-400` (border) | `#eaeaea` | `#2e2e2e` |
| `gray-900` / `gray-1000` | `#4d4d4d` / `#171717` | `#a0a0a0` / `#ededed` |
| `blue-700` (accent / progress) | `#006bff` | `#006efe` |
| `green-700` / `red-700` / `amber-700` | `#28a948` / `#fc0035` / `#ffae00` | `#00ac3a` / `#f13242` / `#ffae00` |

CSS side (Tauri) is a near-literal emission of `--ds-*` names already used in Geist docs (`var(--ds-gray-100)`, etc.). XAML side should use **stable Phlegethon keys** (e.g. `GeistBackground100`, `GeistGray1000Brush`) generated from the same names so lookup stays mechanical.

### What *not* to share as code

- `@vercel/geistcn` React trees
- Tailwind class names as a cross-platform API (`text-label-14` is a web consumption path only; the **metrics** behind them are the shared contract)
- WinUI `XamlControlsResources` defaults as the “design system” for Debian

## WinUI 3: reimplement with ResourceDictionaries

### Platform hooks (Microsoft Learn)

- Theme values switch via `{ThemeResource}`; define per-theme values in `ResourceDictionary.ThemeDictionaries` with keys **`Light`**, **`Dark`**, and **`HighContrast`** ([XAML theme resources](https://learn.microsoft.com/en-us/windows/apps/develop/platform/xaml/xaml-theme-resources)).
- Inside theme dictionaries, prefer `{StaticResource}` for local aliases; use `{ThemeResource}` for theme-agnostic system colors (`SystemColor*`, `SystemAccentColor`) — Learn’s documented exception.
- App-wide dictionaries / styles: [ResourceDictionary and XAML resource references](https://learn.microsoft.com/en-us/windows/apps/develop/platform/xaml/xaml-resource-dictionary), [XAML styles + lightweight styling](https://learn.microsoft.com/en-us/windows/apps/design/style/xaml-styles).
- Theme selection: omit `RequestedTheme` to follow the user, or set it explicitly ([Theming in Windows apps](https://learn.microsoft.com/en-us/windows/apps/develop/ui/theming)). High contrast **overrides** app theme.
- Color principles: use color sparingly for hierarchy and interactivity ([Color in Windows](https://learn.microsoft.com/en-us/windows/apps/design/signature-experiences/color)) — compatible with Geist’s restrained palette; do **not** rely on the user’s Windows accent as the primary brand fill if that fights Geist `blue-700`.

### What to reimplement in XAML

| Geist concept | WinUI approach |
| --- | --- |
| Color tokens | `Color` + `SolidColorBrush` entries in Light/Dark theme dictionaries |
| Type ramp | `FontFamily` + `Style` resources for TextBlock/Button (`GeistLabel14`, …) |
| Geist fonts | Package `.ttf`/`.otf` as Content; reference `ms-appx:///…/Geist[…].ttf#Geist Sans` (and Mono) via `FontFamily` resources; set implicit `TextBlock`/`Control` styles |
| Density | `Thickness` / `x:Double` resources for padding, gaps, row height (~40–48 epx for controls) |
| Radii | `CornerRadius` resources (`6`, `12`, `16`) on buttons, list chrome, flyouts |
| Buttons / inputs | Prefer **lightweight styling** brush overrides (`ButtonBackground`, `ButtonBackgroundPointerOver`, …) mapped onto Geist gray/blue steps rather than full control-template forks |
| Progress | Stock `ProgressBar` / `ProgressRing`; recolor via theme brushes / lightweight resources to `blue-700` (active), `green-700` (complete), etc. |
| Lists / queue | `ListView`/`ItemsRepeater` item templates using token brushes — flat rows, hairline `gray-400` dividers, no heavy elevation |

Segoe UI Variable remains the Fluent default ([Typography in Windows](https://learn.microsoft.com/en-us/windows/apps/design/signature-experiences/typography)). For a Geist-inspired product, **embed Geist** and set it as the app UI font so Windows and Debian share the same typeface; keep Segoe only as fallback for missing glyphs if needed.

### High contrast

Ship a `HighContrast` dictionary that **defers to system brushes** (`SystemColorWindowColor`, `SystemColorWindowTextColor`, …) for surfaces and text. Do not force Geist hex into high contrast. Progress and error states should remain distinguishable via system highlight / hot-tracking colors where possible.

## Tauri webview: shared CSS

Tauri is **frontend-agnostic**: it hosts a static mix of HTML/CSS/JS (or WASM) in a webview ([Frontend configuration](https://v2.tauri.app/start/frontend/)). Vite is the recommended SPA toolchain ([Vite guide](https://v2.tauri.app/start/frontend/vite/)). Styling is ordinary CSS in the frontend bundle — there is no separate “Tauri theme API” to map; the shell is the web stack you choose.

Practical mapping:

1. Emit `tokens.css` from the shared SoT (`:root` for light; `[data-theme="dark"]` or `prefers-color-scheme` for dark).
2. Implement a small **recipe layer** (plain CSS or a thin component set) that mirrors Geist *behavior* (40px controls, 6px radius, label-14, gray borders) without depending on `@vercel/geistcn` if that package’s availability/licensing is unclear for desktop bundling. Using geistcn **only** in the Tauri app is acceptable *if* tokens still come from the shared SoT — but WinUI must not become a second invent-as-you-go system.
3. Load Geist via `@font-face` from packaged `woff2`/`otf` in the frontend assets (same release as WinUI embeds).
4. Prefer sRGB hex tokens everywhere; optional `*-p3` / `oklch` accents are web-only niceties and **must not** be required for parity with XAML (WinUI consumes sRGB).

## Progress / queue UI constraints

Phlegethon needs clear transfer progress (Resumable Transfer, Paused / Failed / Retry) without clutter. Map Geist + WinUI guidance as follows:

### Control choice (WinUI Learn)

From [Guidelines for progress controls](https://learn.microsoft.com/en-us/windows/apps/develop/ui/controls/progress-controls):

| Situation | Control |
| --- | --- |
| Known duration / byte progress (download) | **Determinate** `ProgressBar` |
| Unknown duration, non-blocking (probe, resolve) | **Indeterminate** `ProgressBar` |
| Modal wait that blocks the shell | `ProgressRing` (use sparingly; queue UX should stay non-modal) |
| Virtualized list loading | One bar for the collection — **not** a spinner on every row |

Mirror the same semantics in Tauri with a thin determinate/indeterminate bar component using the same token colors.

### Visual language for queue rows

Keep each row to **one job**: title (`label-14`), one meta line (`label-12` / mono for size·speed·ETA), one slim progress track, one primary action cluster (Pause / Resume / Retry). Avoid cards, badges, and multi-stat strips.

| State | Color token | Notes |
| --- | --- | --- |
| Active / downloading | `blue-700` fill on track | Matches Geist accent usage for focus/links |
| Complete | `green-700` | Or remove bar and show quiet check |
| Failed | `red-700` text/icon; track muted gray | Retry is the affordance |
| Paused | `amber-700` or `gray-900` | Prefer amber only if it stays rare |
| Surfaces | `background-100` page, `gray-100` hover row | Borders `gray-400` |

Numbers (bytes, %): prefer **Geist Mono** / `label-13-mono` so columns align; Geist docs call out tabular use for numeric labels.

### Density budget

- Row vertical padding ≈ `spacing.2`–`spacing.3` (8–12px).
- Progress track height stay thin (2–4px visual) so text hierarchy leads.
- No overlapping status chips on the bar (Geist materials: avoid decorative elevation on dense lists).

## Drift risks

| Risk | Why it happens | Mitigation |
| --- | --- | --- |
| Dual hand-edits | Designer tweaks CSS hex; WinUI keeps Fluent defaults | Single SoT + codegen; PR checklist: both emitters updated |
| Fluent accent bleed | `SystemAccentColor` colors ProgressBar on Windows only | Override progress/button lightweight brushes to Geist `blue-700` |
| Typeface split | WinUI ships Segoe; Tauri ships Geist | Embed same OFL font files on both |
| Radius / height mismatch | Fluent controls ≠ 6px / 40px | Explicit `CornerRadius` + height styles; don’t accept control defaults blindly |
| P3-only web colors | CSS uses `oklch` P3; XAML can’t | Author in sRGB hex; P3 optional enhancement |
| High-contrast breakage | Forced Geist gray on HC theme | Dedicated HC dictionary → system colors |
| Component-library temptation | Ship geistcn on Linux, invent XAML clones ad hoc | Share **recipes** (padding, type, color steps); native controls both sides |
| Upstream alpha churn | `design.md` marked `version: alpha` | Pin vendored snapshot; deliberate upgrades |
| Google Fonts shortcut | Convenient but violates research/tooling constraint | Use vercel/geist-font releases only |

## Share vs reimplement (summary)

| Share (one language) | Reimplement per shell |
| --- | --- |
| Color scales + semantic steps | WinUI control templates / lightweight brushes |
| Spacing, radius, control height | Layout panels (`Grid`/`StackPanel` vs CSS flex/grid) |
| Type ramp metrics + Geist font files | Text stacking / input IMEs |
| State→color mapping for queue/progress | `ProgressBar` vs CSS bar |
| Written UI recipes (row anatomy) | `@vercel/geistcn` / any React kit (Tauri-only if used at all) |

## Implications for the hand-off spec

1. Document the **token SoT path** and generation step as a build requirement for both shells.
2. Specify the **app type subset** and queue-row recipe above as normative UI constraints.
3. Require **parity screenshots** (light/dark) of the same queue mock on WinUI and Tauri before calling UI “done” for a milestone.
4. Leave React geistcn optional for Tauri; never block WinUI on it.

## Sources

- Vercel Geist: [introduction](https://vercel.com/geist/introduction), [colors](https://vercel.com/geist/colors), [typography](https://vercel.com/geist/typography), [materials](https://vercel.com/geist/materials), [design.md](https://vercel.com/design.md), [design.dark.md](https://vercel.com/design.dark.md)
- Fonts: [vercel/geist-font](https://github.com/vercel/geist-font) (OFL 1.1)
- WinUI / Windows: [XAML theme resources](https://learn.microsoft.com/en-us/windows/apps/develop/platform/xaml/xaml-theme-resources), [ResourceDictionary](https://learn.microsoft.com/en-us/windows/apps/develop/platform/xaml/xaml-resource-dictionary), [XAML styles / lightweight styling](https://learn.microsoft.com/en-us/windows/apps/design/style/xaml-styles), [Theming](https://learn.microsoft.com/en-us/windows/apps/develop/ui/theming), [Color](https://learn.microsoft.com/en-us/windows/apps/design/signature-experiences/color), [Typography](https://learn.microsoft.com/en-us/windows/apps/design/signature-experiences/typography), [Progress controls](https://learn.microsoft.com/en-us/windows/apps/develop/ui/controls/progress-controls)
- Tauri 2: [Frontend configuration](https://v2.tauri.app/start/frontend/), [Vite](https://v2.tauri.app/start/frontend/vite/)

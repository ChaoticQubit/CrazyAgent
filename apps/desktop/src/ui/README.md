# Desktop UI kit

Shared components for the CrazyAgent desktop app. Import from `src/ui` (the `index.ts` barrel). Styling is plain CSS in `src/style.css`; every color, font, radius, spacing, motion and z-index value is a token in `src/ui/tokens.css`. Never write a literal color or font in a screen or stylesheet: `test/tokens.test.mjs` fails if `style.css` does, and checks WCAG AA for every token pair in both themes.

Brand note: the logo is not final. `BrandMark` renders the text "CrazyAgent" only. When the owner picks a concept from `docs/design/brand/`, change `BrandMark` and the tokens, nothing else.

## Tokens (`tokens.css`)

| Group | Tokens |
|---|---|
| Surfaces | `--bg`, `--surface`, `--surface-2`, `--border` (decorative), `--border-input` (3:1) |
| Text | `--text`, `--text-2`, `--text-3` (all 4.5:1 on every surface) |
| Accent | `--accent` (graphics, 3:1), `--accent-solid` + `--on-accent` (primary button), `--accent-hover`, `--accent-text` (accent-colored text), `--accent-soft`, `--focus` |
| Status | `--ok`/`--ok-soft`, `--warn`/`--warn-soft`, `--danger`/`--danger-soft`, `--danger-solid` + `--on-danger` |
| Type | `--font` (Instrument Sans), `--mono` (JetBrains Mono, for status, counts and labels) |
| Shape | `--space-1..10`, `--radius-control` 6, `--radius-segment` 4, `--radius-panel` 8, `--radius-dialog` 12, `--radius-pill` |
| Motion | `--dur-fast` 120 ms, `--dur-base` 180 ms, `--ease`; both 0 under reduced motion |
| Layers | `--z-sticky`, `--z-drawer`, `--z-dialog`, `--z-menu`, `--z-toast`, `--scrim`, `--elev-1`, `--elev-2` |

Theme: system by default; `applyTheme("light" | "dark" | "system")` in `state.ts` sets `data-theme` on `<html>`.

## Components and props

### `Icon` (`icon.tsx`)
`{ name: IconName; size?: number = 16; label?: string; className?: string; spin?: boolean }`. Inline SVG from the vendored Lucide files in `src/assets/icons`. Decorative (`aria-hidden`) unless `label` is given. `ICON_NAMES` lists every available name. To add an icon: download its SVG from the pinned Lucide tag into `src/assets/icons/`, add the import and map entry in `icon.tsx`; `test/icons.test.mjs` parses every file.

### `ProviderMark` (`icon.tsx`)
`{ provider: string; name: string; large?: boolean }`. Provider or harness id to a monochrome Simple Icons logo, a Lucide icon (`openai_compatible`, `local`) or a two-letter monogram. Decorative; `name` is its tooltip. Logos only identify; never use provider colors.

### `Button` (`button.tsx`)
`{ variant?: "primary" | "secondary" | "quiet" | "danger" | "danger-text"; size?: "small" | "large"; icon?: IconName; pending?: boolean; pendingLabel?: string; iconOnly?: string }` plus native button attributes. `type` defaults to `"button"`. `pending` disables it and shows a spinner and `pendingLabel`. `iconOnly` renders only the icon with that text as its accessible name. For navigation use `<a className="btn primary" href={formatRoute(...)}>`.

### Fields (`field.tsx`)
- `TextField { id; label; value; onValue(value); helper?; error?; maxLength?; counterAt? }` plus native input attributes. Label above, helper and error below (error has `role="alert"`), linked with `aria-describedby`. The counter appears once length reaches `counterAt`.
- `TextArea { id; label; value; onValue; rows? = 3; helper?; error?; maxLength?; counterAt? }`.
- `SecretField { id; label; inputRef; helper?; error?; maxLength? = 8192 }`. Masked, uncontrolled, no autocomplete. Read `inputRef.current.value` once on submit, send it, then set it to `""`. Never put a secret in React state.
- `Select { id; label; value; options: { value; label; disabledReason? }[]; onValue; helper?; error?; hideLabel? }`.
- `SearchField { label; value; onValue }`. Visually hidden label, search icon.
- `ChipInput { id; label; values; onValues; suggestions?; max? = 12; maxLength? = 40; helper? }`. Enter or comma adds, Backspace on empty removes the last, suggestions are quick-add chips. `addChip(values, text, max, maxLength)` is the pure rule.

### Toggles (`toggle.tsx`)
- `Switch { id; label; checked; onChecked; disabledReason?; hideLabel? }`. `role="switch"`. With `disabledReason` it stays focusable, is `aria-disabled`, and shows the reason.
- `Checkbox { label; checked; onChecked; disabled? }`.
- `Segmented<T> { label; value: T; options: { value: T; label; icon? }[]; onValue }`. `role="radiogroup"`, arrow keys.
- `RadioCard { name; value; checked; onChecked(value); title; details?: ReactNode[] }`.

### Feedback (`feedback.tsx`)
- `StatusBadge { word; tone? }`. Always shows the word. `statusTone(word)` maps the fixed status words from DESIGN.md section 2 to `ok | warn | danger | info | neutral`.
- `Notice { tone? = "warn"; icon?; alert?; id? }`. Inline note; with `id` it can take focus.
- `Banner { action? }`. Warning under the page header.
- `Toast { message }`. One at a time; the session clears it after 4 s (`session.showToast`).
- `EmptyState { icon?; title; action?; compact? }`, children are the body.
- `NotReady { what; compact? }`. The plain "Not ready yet" state for an engine call missing from this build. Never show sample data instead.
- `LoadError { message; onRetry }`.
- `Skeleton { shape: "line" | "row" | "card"; count? }`.
- `CapacityMeter { count; max? = 15 }`. "7 of 15" plus 15 cells, `aria-live="polite"`.

### Overlays (`overlay.tsx`)
- `Dialog { open; title; onClose; onBack?; footer?; wide? }`. Native `<dialog>` with `showModal()`: focus trap, Esc calls `onClose`, focus starts on the first field (or `data-autofocus`) and returns to the opener.
- `Drawer { open; title; onClose; footer? }`. 480 px right panel, same behavior.
- `ConfirmDialog { open; title; confirmLabel; cancelLabel?; pending?; onConfirm; onCancel }`, children are the body. Cancel has initial focus.
- `Menu { label; items: { label; icon?; danger?; onSelect }[] }`. "..." button, arrow keys, Esc returns focus.

### Layout (`layout.tsx`, `shell.tsx`, `windowed-list.tsx`)
- `PageHeader { title; description?; actions? }`. Holds the page's `h1`.
- `Tabs { label; tabs: { id; label; href; icon? }[]; current }`. Route links with `role="tab"`; arrow keys, Home and End move. Give the panel `role="tabpanel" aria-labelledby={"tab-" + current}`.
- `Panel { title?; actions?; padded? = true; className? }`.
- `WindowedList { label; count; rowHeight? = 44; overscan? = 10; viewport? = 560; renderRow(index) }`. Renders visible rows only; rows carry `aria-setsize` and `aria-posinset`. `windowRange(...)` is the pure math.
- `AppShell { sidebar; toast? }` and `Sidebar { route; projects?: { id; name; flag? }[]; profileName?; engine; onRetry }`. Owned by core.
- `BrandMark { large? }`. The only place the logo lives.

## Routes (`router.ts`)

`parseRoute(hash)`, `formatRoute(route)`, `navigate(route, replace?)`, `useRoute()`.

| Route | Hash |
|---|---|
| `{ name: "welcome" }` | `#/welcome` |
| `{ name: "home" }` | `#/home` |
| `{ name: "new-project", step }` | `#/projects/new?step=basics` (`basics`, `documents`, `work`, `pool`, `team`) |
| `{ name: "project", id, tab }` | `#/projects/<id>/overview` (`overview`, `conversation`, `agents`, `context`, `settings`) |
| `{ name: "settings", tab, action? }` | `#/settings/accounts?action=add` (tabs `accounts`, `pools`, `harnesses`, `plugins`, `memory`, `profile`; actions `add`, `new`) |

## Screen contracts for v1-desktop-projects

- `src/screens/new-project/index.tsx` exports `NewProjectFlow({ step }: { step: NewProjectStep })`.
- `src/screens/project/index.tsx` exports `ProjectPage({ id, tab, name }: { id: string; tab: ProjectTab; name?: string })`.
- Both render inside `AppShell`'s page column; render your own `PageHeader`.
- Session: `useAppSession()` from `state.ts` gives `engine`, `projects`, `refreshProjects()`, `showToast(message)`, `noteResult(result)` (flags engine down and unconfirmed changes) and `revision` (bumps after reconnect or Check again). `useLoad(read, keys)` loads one engine read and reloads on `revision`.
- Engine seam: `call(command, args)` in `src/api.ts` returns `{ kind: "ok", value } | { kind: "not_ready" } | { kind: "error", code, message }`. `not_ready` means the Tauri command is not in this build. `receiptResult` reads owner-change receipts; `nextGeneration()` issues draft generations.

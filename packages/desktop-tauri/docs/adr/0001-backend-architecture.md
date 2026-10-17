# ADR 0001: Where the main process logic lives in the Tauri port

**Status:** Accepted (2026-10-17)
**Date:** 2026-10-17
**Deciders:** Nata (fork owner)

## Context

Electron's main process is 12,084 lines of TypeScript in
`packages/desktop/src/main`. The renderer reaches it through the 203 channels in
`packages/desktop/src/shared/types/ipc.ts`: 135 from renderer to main, 68 from
main to renderer.

The trial (`packages/desktop-tauri`) puts a stand-in for main inside the editor
page (`src/backend.ts`, 483 lines) and keeps Rust to file read, write, exists and
boot info (`src-tauri/src/main.rs`, 193 lines). It handles about 30 of the 135
renderer to main channels, for one window only.

The main process does two kinds of work.

**App wide**, needing an owner that lives as long as the app:

- window registry and active window (`app/windowManager.ts:133,181-211`)
- choosing which window opens a file, skipping files already open elsewhere
  (`app/windowManager.ts:256`, `windows/editor.ts:453`), fed by each window
  reporting its open files (`mt::window-add-file-path`, `mt::window-tab-closed`,
  `app/windowManager.ts:370,403`)
- preference and user data changes broadcast to every window
  (`app/windowManager.ts:479-494`)
- single instance and handling a second launch with files
  (`index.ts:78`, `app/index.ts:76-112`)
- the settings window as a singleton (`app/index.ts:612-628`)
- recent files (`menu/index.ts:511,568`)
- shutdown after the last window closes (`app/index.ts:119-127`)

**Per window**:

- save, save as, close and the unsaved changes prompt
  (`menu/actions/file.ts:541-592`); quitting with several unsaved windows is
  already one prompt per window, not a coordinated step
- menu check states per window (`menu/index.ts:165`), context menu,
  command routing to the active window

Upstream activity since 2025-10-17: main 70 commits, renderer 154, Muya 350.
Inside main, by commits: menu 25, filesystem 18, app 17, preferences 15 (mostly
new options in `preferences/schema.json`), ipc 14, windows 9, utils 9,
keyboard 6, everything else 4 or fewer. Every upstream main change has to be
ported to the fork by hand.

Some dependencies need a replacement whatever we choose: chokidar, ced,
iconv-lite, keytar, native-keymap, electron-store, electron-window-state and the
ripgrep binary.

Tauri 2 facts that bear on the choice:

- Launches, second launches and file association arguments arrive in Rust
  callbacks (single-instance plugin, https://v2.tauri.app/plugin/single-instance/).
- All windows share one WebKit web context by default
  (`tauri-runtime-wry-2.12.2/src/lib.rs:4714`), but WebKitGTK still runs a web
  process per page, and WebView2 a renderer per page, so every extra webview
  costs a process.
- There is no synchronous call from page to Rust. Per window constants go in
  `initialization_script`, as boot info already does.
- Events are JSON, broadcast (`emit`) or targeted (`emit_to`), suited to small
  messages (https://v2.tauri.app/develop/calling-frontend/).
- Menus, including popups with check items and accelerators, can be built and
  updated from page JS (`@tauri-apps/api/menu`), so the ☰ menu can live in the
  page.
- Rust can hold off app exit and window close (`prevent_exit`, `prevent_close`,
  `tauri-2.12.4/src/app.rs:90,103`).
- Nothing documents a hidden webview as a background host. Whether a hidden page
  keeps its timers running and receives events is untested on both platforms.

## Decision

**Hybrid: Rust owns app wide state and OS work; each window keeps a TypeScript
backend for its own flows.**

Rust (`src-tauri`) owns:

- start up, command line, single instance, file associations, and quitting when
  the last window closes
- the window registry: creating editor and settings windows, window ids, the
  active window, each window's open file list, choosing the window for a file
- preferences and user data: load with defaults from
  `packages/desktop/static/preference.json`, atomic writes, broadcast of changes
  to every window
- recent files
- file IO with encoding detection and atomic writes (chardetng, encoding_rs),
  file watching (notify), the session buffer store
- external processes: ripgrep (as a sidecar), pandoc; the OS keyring later

TypeScript in each window (`packages/desktop-tauri/src`, growing from
`backend.ts`) owns:

- that window's channel handlers, keeping Electron's channel names and payloads
- save, save as, close and the unsaved changes prompt
- the ☰ menu and context menus, built from the existing desktop menu templates
  with the JS menu API, with per window check states
- shortcuts and command routing, as now
- calling Rust with `invoke` for shared state and OS work, and passing Rust
  events on to the renderer through the `electron.ts` stand-in

Rules for the boundary:

- A page may do no more than Electron's renderer could. Note that this is
  already a lot: the preload gives the renderer general file access
  (`mt::fs::read-file`, `write-file`, `unlink`, `move`, `copy`, `readdir`,
  `preload/index.ts:162-177`, handled without path checks in
  `main/ipc/fs.ts:46-75`) and `shell.openPath` on any path. Commands for shared
  state and app logic still work at the level of a task (set these
  preferences, this window opened this file), not as raw building blocks.
  (Corrected 2026-10-17: the first version assumed Electron's channels were
  narrow.)
- Events that Rust sends straight to the renderer reuse Electron's channel
  names, so `electron.ts` can deliver them unchanged.
- Per window constants (boot info, window id, files to open) go in
  `initialization_script`.

## Options considered

| | A: TS in each window | B: hidden TS coordinator | C: all Rust | D: hybrid (chosen) |
|---|---|---|---|---|
| Complexity | Low now, high once there are several windows | Medium: three parts (pages, coordinator, Rust) | High: full rewrite | Medium: two parts and a boundary |
| Runtime cost | None extra | One extra web process or renderer; two hops per request | None extra | None extra |
| Several windows, second launch | Poor: no owner for shared state | Good | Good | Good |
| Porting upstream main changes | Easiest: TS to TS | Easy for logic, plus routing | Hardest: every change becomes Rust | About a third of changes become Rust |
| Language | TS | TS | Rust | TS and Rust |

### A: TypeScript in each window (the trial as is)

**Pros:** already works; one language; upstream ports are close to copies.

**Cons:** nobody owns app wide state. Each window would hold its own copy of the
preferences and write the same file, there is no window registry, and no code is
alive to answer a second launch or run shutdown. Fixing that means electing a
leader window, which breaks when that window closes. Every window also needs
every permission.

### B: Hidden coordinator window running TypeScript

**Pros:** one owner for app wide state, in TypeScript.

**Cons:** an invisible page that costs a full web process on Linux and a
renderer on Windows. Every request travels page to Rust to coordinator to Rust
to page as JSON events. Start up waits for the coordinator page before the first
window opens. Hidden page behaviour (timers, events) is undocumented and
untested. The coordinator still needs Rust for all OS work and for launch
callbacks, so B has three parts where D has two. The main process code uses Node
and Electron APIs throughout, so it would not run unchanged in a page anyway;
B reuses no more code than D.

### C: Everything in Rust

**Pros:** a natural owner for shared state; no extra process; native crates for
watching, encodings and the keyring; one place for logic.

**Cons:** rewrites all 12k lines, including the menu templates and action
routing, which upstream changes most (menu: 25 commits in a year). Each such
change becomes a translation into Rust. Per window flows like the save and close
prompts gain nothing from moving to Rust.

### D: Hybrid (chosen)

**Pros:** shared state has one owner that lives as long as the app, and launch
callbacks already arrive there. No extra webview. The code upstream changes most
(menu, per window save and close, ipc handlers) stays TypeScript and close to
upstream. Native replacements come from Rust crates.

**Cons:** two languages and a boundary to keep narrow. Upstream changes to the
window manager, preference store, watcher and file IO need translating into Rust.
Rust build times: a cold release build takes about 3.5 minutes on the desktop
machine, incremental builds under a minute.

## Trade-off analysis

The work splits along app wide and per window lines, and each side has a
different natural home.

App wide state needs something that outlives every window and receives launch
callbacks. In Tauri that is the Rust process, which exists in every option. B
adds a second long lived owner in a hidden page, but it still depends on Rust
for OS work, so it adds a process and a hop without removing anything.

Per window flows and menus are where upstream changes concentrate, and in
TypeScript they stay close copies of upstream code. Moving them to Rust (C)
would make the fork's upkeep much heavier for no runtime gain.

Measured by upstream file changes in the last year, the modules that move to Rust
in D (app, windows, filesystem, preferences store, buffer store, data center,
cli) account for about 40% of main process file changes. Most preference changes
are new options in `schema.json`, which stays a shared JSON file, so the share
that needs real translation is about a third.

## Consequences

Easier:

- several windows, second launches and "open with" from the file manager
- one preference store for all windows; the settings window becomes one more
  window running the same per window backend
- replacing Node only and native modules with Rust crates

Harder:

- contributors need both TypeScript and Rust
- about a third of upstream main process changes need translating into Rust
- the boundary needs discipline to stay at task level

To revisit:

- if one user action needs many round trips between TS and Rust, move that flow
  wholly to one side
- spellcheck and print to PDF have no Tauri API in any option; they need their
  own decisions
- per window permissions, once windows have distinct roles (the settings window
  does not need to write documents)
- going further than Electron, for example limiting file writes to paths the
  user opened or picked; it would shrink what a script injected through a
  document could do, but needs care with images and exports, so it is its own
  security decision

## Action items

1. [x] Spike: open a second window from Rust with its own `initialization_script`,
   and show a JS popup menu from the custom title bar, on GNOME Wayland and X11.
   Works on both (`c2463d77`); GTK popups show no shortcut labels.
2. [x] Preferences in Rust: store with defaults, atomic writes, broadcast of
   `mt::user-preference` to all windows. Small, and proves a command plus a
   broadcast end to end. Done in `acd695ab`.
3. [x] Window registry and single instance: per window open file lists, opening a
   second launch's files in the best window, a new window command. Done in
   `d2c1ad43`; folders wait for the sidebar.
4. [x] Review the trial's general commands (`read_text_file`, `write_text_file`,
   the `opener` path scope) against the boundary rule and narrow them. Nothing
   to narrow: they are a subset of Electron's `mt::fs::*` and `shell.openPath`;
   see the corrected rule above.
5. [x] ☰ menu in TS from the desktop menu templates, with recent files from Rust.
   Done in `29216094`; the templates and most actions run unchanged, with
   stand-ins for the main process modules they import.
6. [x] File watching and encodings in Rust. Done in `657fc341` (encodings)
   and the watcher commit after it; polling for network drives is still open.
7. [x] Update `TAURI_HANDOVER.md` sections 3 and 8 once this is accepted.

# Handover: MarkText on Tauri (trial)

Written 2026-10-17 when development moved from a headless NixOS box to a machine
with a real desktop. It is meant for you and for a fresh Claude session; it
assumes no memory of the earlier conversation.

## 1. Goal

Run MarkText without Electron, on **Windows and Linux only** (macOS is out of
scope). The chosen route is **Tauri 2**: keep the existing Vue interface and the
Muya editor, drop the bundled Chromium and Node, and use the system webview
(WebView2 on Windows, WebKitGTK on Linux) with a small Rust backend.

Why it looked feasible before starting:

- The renderer (`packages/desktop/src/renderer`, ~26k lines) has no direct
  Electron imports. Since the sandboxing work (#4244) it talks to the backend only
  through one bridge file, `packages/desktop/src/preload/index.ts`.
- The IPC is a typed list of ~175 channels in
  `packages/desktop/src/shared/types/ipc.ts`.
- Muya already runs in a plain browser (its examples package).

Known risks going in:

- Linux uses WebKitGTK, a different engine from Chromium, and editors depend
  heavily on browser editing behaviour.
- On Windows, WebView2 *is* Chromium (Edge), so the memory saving there will be
  smaller than on Linux; the download size saving is the main win.
- This is a fork. Upstream is very active and unlikely to accept a framework
  switch, so main process changes will have to be ported by hand.

## 2. Where things are

| Item | Value |
|---|---|
| Fork | https://github.com/nataloko/marktext |
| Remotes | `origin` = fork, `upstream` = marktext/marktext |
| Branch | `tauri-trial` (branched from `develop` at `b6a88236`) |
| New package | `packages/desktop-tauri/` |
| Other change | `pnpm-lock.yaml` (entries for the new package only) |
| Commit email (local git config) | `93059500+nataloko@users.noreply.github.com` |

**At the time of writing nothing was committed or pushed**, and the repo had no
`user.name` set. Before switching machines: set `user.name`, commit the trial on
`tauri-trial`, and push it to `origin`. The old machine's scratchpad (screenshots,
test result JSON files, the Playwright WebKit setup) has already been wiped; the
results are recorded in section 6 below.

Nothing in `packages/desktop` or `packages/muya` was modified. The Electron app
still builds and runs as before.

## 3. How the trial works

The key idea: **the existing Electron preload runs unchanged inside the Tauri
page**. Vite aliases the `electron` module to a stand-in, so every `window.*`
global and every IPC channel name the renderer uses stays exactly the same.

```
packages/desktop-tauri/
  index.html            Same as the desktop renderer's, CSP extended for Tauri IPC
  vite.config.ts        Mirrors the renderer section of electron.vite.config.ts,
                        plus the alias electron -> src/electron.ts
  shell.nix             NixOS dev shell (Rust, cargo-tauri, WebKitGTK, Xvfb tools)
  src/
    entry.ts            Boot order: startBackend() -> preload -> renderer main.ts
    electron.ts         Stand-in for ipcRenderer, contextBridge, webFrame, webUtils
    backend.ts          Answers what Electron's main process would: boot info,
                        preferences, open, save, close, window controls, shortcuts
    shortcuts.ts        Keydown -> command id, using the desktop keybinding tables
    markdown.ts         Load and save markdown (line endings, BOM, trailing newline)
  src-tauri/
    src/main.rs         ~190 lines: builds boot info, file read/write commands,
                        opens the window
    tauri.conf.json     devUrl http://localhost:1420, bundles deb/appimage/nsis
    capabilities/default.json   Window, dialog and opener permissions
```

### Boot sequence

1. Rust (`main.rs`, `boot_info`) builds a `BootInfo` object (platform, paths, env
   allow list, files from the command line) and injects it as
   `window.__MT_BOOT__` with `initialization_script`, so it exists before any
   page script runs.
2. `entry.ts` awaits `startBackend()` (`backend.ts`): registers channel handlers,
   loads preferences, writes the startup settings into the page URL (the renderer
   reads `udp`, `wid`, `type`, `theme`, `tbs` etc. from the query string, see
   `packages/desktop/src/main/windows/base.ts` `_buildUrlWithSettings`), installs
   shortcuts and window event listeners.
3. `entry.ts` imports the unmodified preload. Its synchronous
   `sendSync('mt::boot-info')` is answered from `window.__MT_BOOT__`.
4. `entry.ts` imports the renderer's `main.ts`.
5. `bootEditor()` waits until the renderer has subscribed to
   `mt::bootstrap-editor` (`whenListening` in `electron.ts`), sends it, then sends
   `mt::open-new-tab` for each command line file.

### Design decisions worth knowing

- **Main process logic lives in TypeScript in the page** (`backend.ts`), with
  Rust providing only primitives (read, write, exists). This was the fastest way
  to a working trial and keeps the channel payloads identical to Electron's.
  It will not stretch to multiple windows as is: each window would run its own
  copy. Deciding where that logic lives long term (TS per window, a hidden
  coordinator window, or Rust) is an open question, see section 8.
- **Events are delivered asynchronously and held until a listener exists**
  (`electron.ts`). Electron never drops main to renderer messages in practice
  because main sends after page load; the stand-in reproduces that.
- **Shortcuts are handled in the page.** In Electron they are menu accelerators
  in main (`packages/desktop/src/main/keyboard/`), so the renderer never sees
  Ctrl+S etc. `shortcuts.ts` reads the same Linux/Windows tables and
  `backend.ts runCommand` maps command ids to the same events main would send.
  Cut, copy and paste are deliberately left to the webview.
- **`startUpAction: 'restoreAll'` is rewritten to `'blank'`** before the renderer
  sees it (`rendererPreferences` in `backend.ts`). Session restore isn't
  implemented, and with `restoreAll` the renderer would close the window without
  asking about unsaved tabs. Remove this once restore exists.
- **`titleBarStyle` is forced to `custom`** and the window has no system
  decorations. The custom title bar uses `-webkit-app-region: drag`, which only
  Chromium understands, so `backend.ts` starts a window drag on mousedown inside
  `.title-bar` (double click toggles maximise).
- **Tauri's native drag and drop handler is disabled** so HTML drop events reach
  the editor. Side effect: dropped files have no filesystem path yet.
- **Non UTF8 files are refused**, not guessed. `read_text_file` errors on invalid
  UTF8 so a save can never re-encode a file wrongly. Electron uses ced + iconv.
- **Minify is off**, matching electron-vite's renderer default. It also avoids a
  build failure: `components/titleBar/index.vue` has an invalid
  `var(sideBarTitleColor)` (missing `--`) that the CSS minifier rejects. That's an
  upstream bug worth a separate fix.
- **Dev server port is 1420**, not 5174. Muya's Playwright suite uses 5174 with
  `reuseExistingServer`, so anything else on 5174 silently hijacks its tests.
- **Debug builds print the page console to the terminal** via WebKitGTK's
  `enable-write-console-messages-to-stdout` (`main.rs`, Linux only). Unhandled
  channels show up there as `[tauri] no send handler for "..."`, which is the
  quickest way to find what to implement next.
- Settings live in Tauri's app data dir (Linux:
  `~/.local/share/com.github.marktext.tauri/`), separate from normal MarkText's
  `~/.config/marktext`.

## 4. Setting up on the new machine

### Get the code

```bash
git clone https://github.com/nataloko/marktext.git
cd marktext
git remote add upstream https://github.com/marktext/marktext.git
git switch tauri-trial
git config user.email 93059500+nataloko@users.noreply.github.com
```

### Linux prerequisites

NixOS: everything is in `packages/desktop-tauri/shell.nix`
(`nix-shell packages/desktop-tauri/shell.nix`). pnpm is in it too.

Other distros (from Tauri's prerequisites page; check it if a package name has
moved):

- Debian/Ubuntu: `libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev`
- Fedora: `webkit2gtk4.1-devel openssl-devel curl wget file libappindicator-gtk3-devel librsvg2-devel libxdo-devel` plus the development tools group
- Arch: `webkit2gtk-4.1 base-devel curl wget file openssl appmenu-gtk-module libappindicator-gtk3 librsvg xdotool`

Then Rust via rustup, Node >= 20.19, pnpm 10, and the Tauri CLI:
`cargo install tauri-cli --version "^2" --locked`.

### Windows prerequisites (untested so far)

Visual Studio Build Tools (C++ workload), Rust (MSVC toolchain), Node, pnpm,
`cargo install tauri-cli --version "^2" --locked`. WebView2 ships with Windows
10/11. The bridge already picks `keybindingsWindows.ts` and win32 path handling
when `platform === 'win32'`.

### Install JS dependencies

```bash
pnpm install --ignore-scripts
```

`--ignore-scripts` skips the root postinstall, which downloads Electron and
rebuilds its native modules; the Tauri build needs neither. On a normal distro a
plain `pnpm install` also works if you want the Electron app for comparison.

### Run

Development (two terminals, the dev server must be up first):

```bash
pnpm --filter marktext-tauri dev:web          # Vite on http://localhost:1420
cd packages/desktop-tauri && cargo tauri dev   # debug app; open a file with: cargo tauri dev -- -- path/to/file.md
```

Note: the dev server serves thousands of separate modules, which WebKitGTK loads
noticeably slower than a bundle. Judge speed only on a built version.

Built version:

```bash
cd packages/desktop-tauri
pnpm build:web
cargo tauri build --no-bundle            # release binary
cargo tauri build --bundles deb          # or a .deb (appimage and nsis are configured too)
cargo tauri build --debug --no-bundle    # debug binary with embedded assets
src-tauri/target/release/marktext-tauri path/to/file.md
```

`cargo build` on its own produces a binary that loads `devUrl`, not the bundled
files. On the old 4 core machine a cold debug build took ~15 min and a cold
release build (LTO, opt-level s) ~33 min; incremental builds take under a minute.

### Checks

```bash
node_modules/.bin/eslint packages/desktop-tauri/src packages/desktop-tauri/vite.config.ts
```

ESLint is clean. The repo's Prettier config adds `async (` but ESLint wants
`async(`; ESLint is what CI enforces, so run `eslint --fix` after Prettier.
There is no typecheck for the new package yet (the entry pulls in the whole
renderer, which needs the desktop tsconfig).

## 5. What works (tested on Linux, release and debug builds)

Tested on a virtual display (Xvfb) on the old machine, driven with xdotool:

- Boot to an editor; opening a file given on the command line
- Rendering: headings, bold/italic/inline code, lists, tables, KaTeX math
- Custom title bar with path, word count, unsaved dot, window buttons
- Typing, Enter for new paragraphs
- Ctrl+B on a selection (also shows Muya's floating format toolbar)
- Undo (Ctrl+Z) and redo (Ctrl+Shift+Z)
- Source code mode (Ctrl+E, CodeMirror)
- New tab (Ctrl+T), tab bar
- Save (Ctrl+S) to an existing file; save of a new tab through the GTK save
  dialog; tab renamed afterwards
- Close button with unsaved changes asks "Close without saving?" (No keeps the
  window open)

Not verified: Ctrl+B with nothing selected did nothing. Not checked whether
Electron behaves the same.

## 6. Measurements and test results

### Electron vs Tauri on the same machine and file

| | Electron MarkText 0.19.1 (nixpkgs) | Tauri trial (release) |
|---|---|---|
| Size | ~455 MB installed (355 MB Electron + 100 MB app) | 10.5 MB .deb, 14 MB binary |
| Memory (PSS, whole process tree, one file open) | 372 MB | 258 MB |
| Launch until the file's name is in the title | 4.9 to 6.4 s | 4.8 to 6.7 s |

Caveats: the machine had load averages of 8 to 14 on 4 cores, so timings are
rough. The Tauri size does not include WebKitGTK, which comes from the system.
The Electron build is an older release than this repo (0.22.0-dev). Both ran on
Xvfb without a GPU. **Re-measure on the desktop machine**, ideally against the
Electron build of this same branch.

### Muya's browser tests on WebKit

Muya's Playwright suite (`packages/muya/e2e`) run on Chromium and WebKit:

| Run | Tests | Passed | Failed | Skipped |
|---|---|---|---|---|
| Chromium | 476 | 469 | 1 | 6 |
| WebKit (suite's ignore list on) | 462 | 400 | 15 | 47 |
| WebKit (ignore list off) | 476 | 408 | 21 | 47 |

- The browser was Playwright's WebKit (WPE MiniBrowser), the same engine family
  as WebKitGTK but not the exact webview Tauri uses.
- The one Chromium failure (`stability/perf.spec.ts`, 10k paragraphs) was load.
- Passing on WebKit: typing, selection, lists, undo, inline format shortcuts and
  the format toolbar.
- The three files in the config's WebKit ignore list
  (`inline/format-toolbar.spec.ts`, `inline/shortcuts.spec.ts`,
  `editing/search-replace.spec.ts`) all **passed** on WebKit, so that ignore
  list looks stale. Could be an upstream contribution.
- **Fails every time on WebKit (real editing bug):** Enter in a code block puts
  the caret on the wrong line (`blocks/codeblock-trailing-newline-5114.spec.ts`,
  3 tests, e.g. expected line 3, got 5).
- Fails every time, minor features: `&nbsp;` and entity widths and arrow up
  column (`editing/nbsp-nowrap.spec.ts`, `editing/arrow-column-3412.spec.ts`,
  probably font metrics); footnote backlinks (`ui/footnote.spec.ts`); frozen
  table rectangle selection leaves an active block
  (`editing/table-rect-clipboard.spec.ts`); text and background colour spans not
  merged (`inline/format-color.spec.ts`); Mermaid from fence autocomplete never
  renders (`blocks/diagram-fence-lang-5060.spec.ts`, timing sensitive).
- Flaky, probably load: dropped characters in typing tests ("hell" for "hello"
  in `editing/undo-redo.spec.ts`, "##### Ti" in
  `typing/paragraph-and-headings.spec.ts`, "Worlde" in
  `ui/quick-insert-table-after-undo-5355.spec.ts`). They passed when rerun one
  at a time, but an input timing bug isn't ruled out.
- **Not tested on WebKit at all:** paste (the suite skips clipboard tests on
  WebKit, "ClipboardItem text/html unreliable") and IME (needs Chrome DevTools
  Protocol). These are where WebKitGTK is most likely to differ.

How it was run on NixOS (Playwright 1.64 from the repo): browsers from
`nixpkgs#playwright-driver.browsers` (built for 1.63) with a directory of
symlinks named for the revisions 1.64 expects (chromium-1248,
chromium_headless_shell-1248, webkit-2370), `PLAYWRIGHT_BROWSERS_PATH` pointing
at it, `PLAYWRIGHT_SKIP_VALIDATE_HOST_REQUIREMENTS=true` and
`PLAYWRIGHT_USE_BUNDLED_CHROMIUM=1`. WebKit also needed a patched
`minibrowser-wpe/MiniBrowser` wrapper with mesa and libglvnd on the library path,
`__EGL_VENDOR_LIBRARY_DIRS=<mesa>/share/glvnd/egl_vendor.d`,
`LIBGL_ALWAYS_SOFTWARE=1` and `EGL_PLATFORM=surfaceless`, because that box had no
GL driver. On a desktop with a GPU, or a distro where `npx playwright install`
works, none of this should be needed. Command:
`pnpm -C packages/muya/e2e exec playwright test --project=webkit`.

## 7. Not implemented yet

Each item lists where the Electron version lives, as the starting point for a
port. Unhandled channels are logged in debug builds (section 3).

| Missing | Electron code / channel |
|---|---|
| ☰ application menu (button does nothing) | `mt::menu::popup-application`, `src/main/menu/` |
| Right click context menus (untested) | `src/main/contextMenu/`, Electron `context-menu` event in `src/main/windows/editor.ts` |
| Sidebar, open folder, file tree | `mt::ask-for-open-project-in-sidebar`, `src/main/menu/actions/file.ts`, `src/main/filesystem/watcher.ts` |
| Reload when a file changes on disk | `mt::update-file`, `src/main/filesystem/watcher.ts` (chokidar; Rust `notify` crate) |
| Encodings other than UTF8 | `src/main/filesystem/encoding.ts`, `markdown.ts` (ced + iconv; Rust `chardetng` + `encoding_rs`) |
| Spellcheck | `src/main/spellchecker/`, `src/main/contextMenu/editor/spellcheck.ts` |
| Export to PDF/HTML, print | `src/main/menu/actions/file.ts` (`printToPDF`, `print`) |
| Images: paste, drop, insert from file | `mt::fs::*`, `mt::clipboard::*`, `webUtils.getPathForFile`; Tauri drag drop event |
| Find in folder | `mt::rg::*` (ripgrep; can ship as a Tauri sidecar binary) |
| Recent files | `src/main/menu/` |
| Multiple windows | `src/main/app/windowManager.ts` |
| Session restore | `update-buffer-state`, `src/main/editorBufferStore/` (then drop the `restoreAll` rewrite) |
| Preferences window | renderer route `/preference`, window type `settings`, `src/main/windows/setting.ts` |
| User keybindings (`keybindings.json`) | `src/main/keyboard/shortcutHandler.ts` |
| Auto update | `electron-updater`; Tauri updater plugin |
| Window size/position memory | `electron-window-state`; Tauri window state plugin |
| Link titles on URL paste, PlantUML, uploads, fonts list | `mt::link::fetch-html`, `mt::diagram::fetch-plantuml`, `mt::uploader::upload`, `mt::fonts::list` |

Paths in this table are relative to `packages/desktop/`.

## 8. Next steps (suggested order)

1. **Hands on test on the desktop** (Linux first). Things a virtual display
   could not show:
   - typing feel, scrolling, large documents
   - paste: plain, rich text from a browser, lists, images
   - IME (Chinese/Japanese input) and dead keys
   - HiDPI and fractional scaling
   - resizing the window from its edges (it has no system decorations, which on
     some window managers means no resize border)
   - whether the save dialog blocks typing in the editor while it is open
   - Wayland as well as X11
2. **Code block Enter bug** on WebKit: reproduce in the Tauri app, then in Muya.
3. **Windows build**: build on a Windows machine, or add a GitHub Actions
   workflow on the fork (Tauri's `tauri-action` can build an NSIS installer).
4. **Decide the backend architecture** for the full port (section 3, first design
   decision) before porting multi window or session features.
5. Work through section 7, starting with the ☰ menu (the only way to reach most
   commands on Linux/Windows besides shortcuts and the command palette,
   Ctrl+Shift+P), file watching, and encodings.
6. Fork upkeep plan: the renderer and Muya merge from upstream untouched; every
   upstream change to `packages/desktop/src/main` needs a matching change in
   `backend.ts` or Rust. Worth a script that lists main process changes since
   the last sync.

## 9. Lessons from the old machine

- `WEBKIT_DISABLE_COMPOSITING_MODE=1` (and `LIBGL_ALWAYS_SOFTWARE=1`) made
  WebKitGTK 2.54 paint a blank white window while the page ran normally. Don't
  set them unless you have a reason.
- Under a bare X server with no window manager, GTK dialogs don't take keyboard
  focus by themselves. Not an issue on a real desktop.
- Port 5174 belongs to Muya's tests (section 3).
- A blank window in dev mode can just be slow module loading; check the console
  output in the terminal before debugging.
- Untested: whether the custom title bar drag replacement feels right on a real
  window manager.

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

The trial was committed on `tauri-trial` and pushed to `origin` on 2026-10-17,
after moving to the desktop machine. The old machine's scratchpad (screenshots,
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
    boot.ts, preferences.ts, files.ts, window.ts
                        Split out of backend.ts: boot info, the window's copy of
                        the preferences, open and save, window controls
    appMenu.ts          The ☰ menu, built from the Electron menu templates
    stand-ins/          Browser versions of the main process modules those
                        templates import (see its README)
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
  copy. **Decided 2026-10-17** in
  `packages/desktop-tauri/docs/adr/0001-backend-architecture.md`: Rust owns app
  wide state and OS work (start up, single instance, window registry,
  preferences, recent files, file IO and watching, session store); each window
  keeps a TypeScript backend for its own flows (its channel handlers, save and
  close, menus, shortcuts).
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
- **Encodings never lose data** (`src-tauri/src/encoding.rs`, since the step 6
  work). Detection follows Electron (BOM, then valid UTF-8, then a guess, with
  `chardetng` in place of ced), but where iconv-lite silently replaces, the trial
  refuses: a file whose bytes do not fit the detected or default encoding is not
  opened, and a character the file's encoding cannot hold makes the save fail
  with a message, so a round trip never damages a file.
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

Fedora Atomic (the current desktop machine): everything runs in a distrobox
named `marktext`, made from `registry.fedoraproject.org/fedora-toolbox:44`, with
the Fedora packages above plus `gcc gcc-c++ make rust cargo nodejs npm` from dnf
and `sudo corepack enable` for pnpm. `cargo-tauri` lands in `~/.cargo/bin`, so
add that to `PATH` inside the box. Run every command through
`distrobox enter marktext -- ...`; the app opens on the host desktop from there.

### Windows prerequisites

Visual Studio Build Tools (C++ workload), Rust (MSVC toolchain), Node, pnpm.
WebView2 ships with Windows 10/11. The bridge already picks
`keybindingsWindows.ts` and win32 path handling when `platform === 'win32'`.

The Tauri CLI is also a dev dependency of the package (`@tauri-apps/cli`), so
`pnpm --filter marktext-tauri exec tauri build` works without
`cargo install tauri-cli`; both run the same CLI version.

GitHub Actions: `.github/workflows/tauri-windows.yml` runs on pushes to
`tauri-trial` that touch the trial, the desktop sources or the lockfile, and on
demand. It runs the Rust tests, Clippy with warnings as errors, the type check,
and builds the NSIS installer, uploaded as the `marktext-tauri-windows-x64`
artifact of the run, plus a debug installer (`-debug`, DevTools on
Ctrl+Alt+I) for looking into Windows only problems.

Windows results, 2026-10-18, in a Windows 11 VM (the user's own test, then
mine): install, open a file, second launch opens a tab in the running app,
title bar drag and double click to maximise, Edit menu cut, copy and paste,
typing after closing a menu. Two Windows only fixes came out of it:
- WebView2 refuses `document.execCommand('paste')`, so the Edit menu pastes
  through the DevTools protocol (`Input.dispatchKeyEvent` with the `paste`
  command, `main.rs run_editing_command`), Chromium's equivalent of
  Electron's `webContents.paste()`; it keeps rich HTML paste.
- Pressing a title bar element that cannot take focus, such as the ☰ button,
  moved Chromium's page focus from the editor to `<body>`, and Muya ignores
  keys and paste while it lacks focus (`muya.ts hasFocus`). Native focus
  was fine (wry hands it back to WebView2), which is why Tauri's `setFocus`
  and WebView2's `MoveFocus` did not help. `window.ts` now cancels the
  default action of a mouse press on the title bar (not on text fields).
  Found with the debug build's DevTools: `document.activeElement` was BODY.

Testing in the local VM without an agent inside Windows:
`packages/desktop-tauri/scripts/windows-vm.py` takes SPICE screenshots and
sends QEMU mouse and keyboard events (UK layout). Copy the installer into
the VM's shared folder (host `~/VMs/shared`, `Z:` in Windows), install with
`Win+R`, `Z:\...\MarkText_0.22.0-dev_x64-setup.exe /S`, and start
`%LOCALAPPDATA%\MarkText\marktext-tauri.exe`. A running instance catches
launches of a new one (single instance), so close it before testing.

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
On the 16 core desktop a cold release build takes ~3.5 min.

### Checks

```bash
node_modules/.bin/eslint packages/desktop-tauri/src packages/desktop-tauri/vite.config.ts
pnpm --filter marktext-tauri typecheck
(cd packages/desktop-tauri/src-tauri && cargo test && cargo clippy --all-targets)
```

The existing Rust code is not formatted with rustfmt defaults, so `cargo fmt`
would reformat whole files; keep to the surrounding style instead.

ESLint is clean. The repo's Prettier config adds `async (` but ESLint wants
`async(`; ESLint is what CI enforces, so run `eslint --fix` after Prettier.
`pnpm --filter marktext-tauri typecheck` type checks the package's own files
against the desktop tsconfig. It leaves out `src/entry.ts`, which pulls in the
whole renderer (that needs vue-tsc and Muya's built types).

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

Hands on test on a real desktop, 2026-10-17 (Fedora 44, GNOME on Wayland,
WebKitGTK 2.54.1, release build): a checklist covering section 8 steps 1 and 2 (rendering,
typing, paste, code block Enter, window resize and drag, save dialog, close
prompt, scaling) was tried by hand and reported as all working. No detailed
notes per item.

Spike for ADR 0001, 2026-10-17 (commit `c2463d77`), checked on Xvfb with
xdotool and by hand on GNOME Wayland:

- Extra editor windows from Rust (`new_editor_window`), each with its own boot
  info and window id, opened from the ☰ menu or Ctrl+N. Closing windows one by
  one keeps the app running; closing the last one exits.
- Native popup menus from page JS (`src/menu.ts`): the generic
  `mt::menu::popup` channel (tab and sidebar right click menus) and a
  placeholder ☰ menu. Placement is right on X11 and Wayland, clicks reach the
  renderer, and reopening after Escape works.
- Gap: GTK popup menus show no shortcut labels (Electron shows "Ctrl+T" etc.).

Preferences in Rust, ADR step 2 (commit `acd695ab`), checked on Xvfb:
`src-tauri/src/preferences.rs` owns `preferences.json` (same app data dir as
before). It applies Electron's start up rules (drop unknown keys, add new ones,
the `lastState` migration, dark theme on first run if the page reports a dark
system theme) and broadcasts each real change to every window as
`mt::user-preference`. A theme change from the command palette in one window
restyles all windows and survives a restart. Unchanged values are neither
written nor broadcast; every window saves `sideBarVisibility` while booting
(`renderer/src/store/layout.ts:76`), so this keeps new windows quiet.
`cargo test` in `src-tauri` covers the load rules. Not ported: schema
validation and system language detection on first run.

Window registry and single instance, ADR step 3 (commit `d2c1ad43`), checked
on Xvfb with a second launch for each case: `src-tauri/src/windows.rs` keeps
each window's open files (reported by the page when it opens, saves under a new
name or closes a tab) and the focus order. A second launch hands its arguments
to the running app (tauri-plugin-single-instance, over D-Bus on Linux) and exits
in about 0.3 s. Its files go to one window when only one is open, otherwise to
the window holding files from the same folder, otherwise to the last focused
one; `--new-window` and the `openFilesInNewWindow` preference open new windows;
no files just raises the last window. Deliberate difference: a file already open
in some window goes to that window, which switches to its tab (Electron means to
skip it, but its scoring only notices files open in the first window). Not done:
files that do not exist yet (passed on, so the
page shows "Cannot open tab"; Electron opens them as named empty documents),
and raising windows on Wayland is untested. Note that any running build,
debug or release, now catches launches of the other.

The ☰ menu, ADR step 5 (commits `29216094`, `9bc906a3`), checked on Xvfb: the
Electron menu templates and most menu actions run unchanged in the page; the
modules they import that need Node or Electron resolve to `src/stand-ins`
through a Vite plugin. The menu is rebuilt each time it opens and the
renderer's latest menu updates are replayed onto it, so check marks and greyed
out items follow Electron (checked: Bold after Ctrl+B, Source Code Mode with
Typewriter and Focus greyed and Format greyed, Always on Top, Follow System
Theme and a theme change through the reused theme actions, Open Recent from
Rust's `recently-used-documents.json`). Quit now closes every window, each
asking about its own unsaved documents. Not available yet, with a notice:
pandoc import and export, Check for Updates.
Known problems:
- Cut, Copy and Paste in the Edit menu first did nothing: GTK's built in items
  do not act on the WebKit page. Fixed by `main.rs editing_command`, which runs
  WebKitGTK's editing command (the counterpart of Electron's
  `webContents.copy()`); checked on Xvfb. On Windows it runs
  `document.execCommand`, untested; WebView2 may refuse a paste there.
- The popup shows no shortcut labels (only GTK's built in items did).
- On Xvfb, once, a reopened menu only appeared after the next key press; not
  reproduced in reruns. Xvfb has no window manager, so check on a desktop.
- Once, the app closed without any log line after a long session in source
  code mode that used menu Copy and Paste and Ctrl+C; not reproduced.
- On Linux the popup call returns only when the menu closes (muda runs a
  nested GTK loop, `platform_impl/gtk/mod.rs` `show_context_menu`), contrary
  to the Tauri JS docs. That could let renderer context menus get
  `mt::menu::closed` on dismissal too; not used yet.

Encodings, ADR step 6 part 1, checked on Xvfb: GBK, Windows-1252 and UTF-16
with BOM files open with the right text and save back in the same encoding
(BOM kept); typing Ω into the Windows-1252 file makes the save fail with
'"Ω" cannot be saved in cp1252' and leaves the file untouched. Saving also
flushes to disk, keeps the file's permissions, writes through a link to its
target and retries briefly when Windows holds the file; a name typed without
an extension in the save dialog gets ".md". Not done: owner preservation,
UTF-32 BOM detection (as in Electron), files that do not exist yet.

File watching, ADR step 6 part 2, checked on Xvfb: `src-tauri/src/watch.rs`
watches the folder of every open file (so replace by rename is seen), waits
until a file has been quiet for a second, and reports it to each window whose
own last read or write of it had different content; so a window's own save is
quiet for that window but reaches other windows with the file open, as with
Electron's per window ignore list (without its 1.3 s time window). The page loads the file and
sends Electron's `mt::update-file` payloads, so the renderer behaves as before:
an unmodified tab reloads silently, a modified one asks, a deleted file warns,
and a recreated one asks again. Watches follow the window registry (open,
save under a new name, close tab, close window). Hitting the OS watch limit
shows Electron's "inotify limit reached" notice once.

Review of steps 5 and 6 (reviewer agent, 2026-10-17) and what came of it:
- Fixed: a save in one window hid the change from other windows with the same
  file open (per window hashes now, `db7f9508`); legacy text that would not
  save back byte for byte (GBK A2 E3 is "€", which GBK writes as 80) now
  refuses to open (`1ae638be`); temp files are unique, created with the
  target's mode and retried on Windows sharing violations (`aa38507c`); links
  open as their target so their folder is watched; watch keys ignore case on
  Windows; a folder the OS refused is retried by the next file.
- Open: the owner of a saved file is not kept (write-file-atomic chowns); the
  window registry compares paths case sensitively on Windows.
- Done since: the file read and write commands are async (2026-10-18), so a
  slow drive no longer freezes every window's main thread.

Preferences window (commit `70497cd5`), checked on Xvfb: Rust
`open_settings_window` keeps one settings window (950x650, no minimise,
centred, as Electron's preferencesWinOptions); opening it again brings it
forward and sends `settings::change-tab`. Its boot info says
`windowType: "settings"`, so the page sets `type=settings` in the URL (the
renderer routes to /preference), skips documents, menu and editor shortcuts,
and closes at once, without the unsaved documents prompt. Opened from the ☰
menu, Ctrl+, and the renderer's `mt::open-setting-window`. Preferences go
through the Rust store, so a theme picked in settings restyles every window
and persists; quitting closes settings too. Working: General (including the
default folder picker), Editor, Markdown, Theme; Keybindings lists the
defaults. Not yet: saving custom shortcuts (shows a notice), the Image page's
user data (image folder, uploaders: `mt::set-user-data`, dataCenter), the
fonts list, pandoc and command detection (read as missing), spellchecker
dictionaries (empty lists).

Folders, part 1 of the sidebar (2026-10-18), checked on Xvfb:
`src-tauri/src/folders.rs` gives each window with a folder a thread that lists
it and then follows it with `notify`: on Linux one inotify watch per listed
folder, so skipped folders such as node_modules cost none; on Windows one
recursive watch. It does not rely on what each event says: it lists the
folders that had events again and sends the differences, in batches, as
Electron's `mt::update-object-tree` changes (`src/folders.ts` passes them on).
What is shown follows `watcher.ts`: every folder except node_modules and
Electron archives, markdown files only, minus `treePathExcludePatterns`. A
folder opens from the ☰ menu, Ctrl+Shift+O, the sidebar's button, the recent
list (which now holds folders too), the command line, a second launch (a
folder already shown brings its window forward; `_openPathList` is ported as
`windows.rs plan`) and the start up options "open the default folder" and
"open the last folder". Opening one sets `lastOpenedFolder`, joins the recent
list and follows `openFolderInNewWindow`; routing prefers the window whose
folder holds a file. Checked: live add, remove and rename of files and
folders, a folder replaced under the same name, a page reload, 5,000 files in
100 folders (shown in about 4 s). Differences from Electron: add changes carry
no file content (Electron reads every file in the tree), links to folders
inside the tree are not followed (that folder would need a second inotify
watch), and globset lets `*` match a leading dot where minimatch does not.
Not checked: picking a folder in the GTK dialog (it gets no keyboard focus on
Xvfb), Windows. A reviewer pass found, and the commit fixes: a folder replaced
under the same name lost its watch; links to a folder already shown shared one
inotify watch; notify on Windows drops overflowing events without a word (a
busy round now lists everything again); an older folder could win a race with
a newer one; a page reload lost the sidebar.

Upstream renderer bug seen in passing: the command palette's Change Theme list
shows ids ("window.change-theme-light") instead of names, because
`getCommandsWithDescriptions` (`renderer/src/commands/index.ts:724`) sets the
theme names and then its recursive call overwrites them with
`getCommandDescriptionById`, which returns the id for unknown commands. Same
code in Electron; worth a fix upstream.

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
| ☰ menu: pandoc, updates (see section 5) | `src/main/menu/actions/` |
| Editor right click menu (tab and sidebar menus work through `mt::menu::popup`) | `src/main/contextMenu/`, Electron `context-menu` event in `src/main/windows/editor.ts` |
| Sidebar file operations: new file or folder, rename, move, copy, paste, delete to trash; the menu's Rename and Move To. A file created in the sidebar must arrive with its content (`data` in the add change), which the renderer opens as the new tab | `mt::fs::*`, `mt::fs-trash-item`, `mt::rename`, `mt::response-file-move-to`, `renderer/src/store/project.ts` |
| Folder drag and drop onto the window | `mt::window::drop`, `webUtils.getPathForFile`; Tauri drag drop event |
| File watching: polling for network drives, `watcherUsePolling` | `src/main/filesystem/watcher.ts` (notify's PollWatcher) |
| Spellcheck | `src/main/spellchecker/`, `src/main/contextMenu/editor/spellcheck.ts` |
| Export to PDF/HTML, print | `src/main/menu/actions/file.ts` (`printToPDF`, `print`) |
| Images: paste, drop, insert from file | `mt::fs::*`, `mt::clipboard::*`, `webUtils.getPathForFile`; Tauri drag drop event |
| Find in folder and quick open | `mt::rg::*`, `mt::open-file-by-window-id` (ripgrep's crates can run in process) |
| Missing CLI files as named empty documents | `src/main/filesystem/markdown.ts loadCliMarkdownFile` |
| Session restore | `update-buffer-state`, `src/main/editorBufferStore/` (then drop the `restoreAll` rewrite) |
| Settings pages: user data for the Image page (image folder, uploaders), fonts list, pandoc and command detection | `src/main/dataCenter/`, `src/main/ipc/fonts.ts`, `src/main/ipc/cmd.ts` |
| User keybindings (`keybindings.json`): save from the Keybindings page, apply to shortcuts and menu | `src/main/keyboard/shortcutHandler.ts`, `src/main/app/index.ts` mt::keybinding-* |
| Auto update | `electron-updater`; Tauri updater plugin |
| Window size/position memory | `electron-window-state`; Tauri window state plugin |
| Link titles on URL paste, PlantUML, uploads, fonts list | `mt::link::fetch-html`, `mt::diagram::fetch-plantuml`, `mt::uploader::upload`, `mt::fonts::list` |

Paths in this table are relative to `packages/desktop/`.

## 8. Next steps (suggested order)

Steps 1, 2 and 4 were done on 2026-10-17 (see section 5 and ADR 0001). The
order from here follows the ADR's action items: a spike for a second window and
a JS popup menu, preferences in Rust, the window registry and single instance,
then the ☰ menu, file watching and encodings.

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
3. **Windows build**: GitHub Actions workflow added on 2026-10-18 (section 4).
   Then test the installer by hand on Windows.
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

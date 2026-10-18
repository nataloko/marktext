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
- **With `startUpAction: 'restoreAll'` (the default) a window with unsaved tabs
  closes without asking**, as in Electron (renderer `store/editor.ts`
  LISTEN_FOR_CLOSE): the session store (`src-tauri/src/buffers.rs`) is then the
  only copy of that text, so changes there need care (section 5).
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
# Windows only code, checked from Linux (no linking, so no Windows SDK needed):
(cd packages/desktop-tauri/src-tauri && CARGO_TARGET_DIR=target/wincheck cargo clippy --target x86_64-pc-windows-gnu)
```

The Windows check needs Fedora's `rust-std-static-x86_64-pc-windows-gnu`,
`mingw64-binutils` and `mingw64-gcc` in the distrobox (installed on
2026-10-18). The CI still builds the real MSVC installer.

The same toolchain builds a Windows debug app that runs in the VM, which is
much quicker than a CI round when trying a change:

```bash
pnpm --filter marktext-tauri build:web
(cd packages/desktop-tauri/src-tauri && CARGO_TARGET_DIR=target/wincheck cargo tauri build --debug --no-bundle --target x86_64-pc-windows-gnu)
# copy target/wincheck/x86_64-pc-windows-gnu/debug/{marktext-tauri.exe,WebView2Loader.dll}
# into a folder in ~/VMs/shared/marktext-tauri-tests and run the exe there
```

It opens a console window that shows Rust's `eprintln!` output, and
Ctrl+Alt+I opens the page's DevTools. Close any other MarkText first (single
instance).

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
Xvfb). Checked in the Windows VM with the CI installer (`0fbd0829`): a folder
from the command line, live add, delete, rename and a case only rename made
inside Windows, opening a file from the tree, a second launch with the same
folder, and the Windows folder picker (Ctrl+Shift+O). The VM's shared folder
(`Z:`) reports no changes made from the Linux side; changes made inside
Windows show up, and with them anything the Linux side changed in the same
folder. A reviewer pass found, and the commit fixes: a folder replaced
under the same name lost its watch; links to a folder already shown shared one
inotify watch; notify on Windows drops overflowing events without a word (a
busy round now lists everything again); an older folder could win a race with
a newer one; a page reload lost the sidebar.

Sidebar file operations, part 2 of the sidebar (2026-10-18), checked on
Xvfb: `src-tauri/src/fileops.rs` does the renderer's `window.fileUtils` calls
(is-file, is-directory, is-executable, ensure-dir, write-file, output-file,
read-file, readdir, copy, move), moves to the trash (`trash` crate) and the
menu's Rename and Move To; `src/fileOps.ts` asks Electron's questions first
(Move to Trash, Replace). Checked: New File (the new file opens as a tab,
saves, and is watched), New Directory, Copy and Paste in the same folder
("alpha (copy).md"), Cut and Paste into another folder, Rename in the sidebar
and from the ☰ menu (the open tab follows, its watch moves with it, so an
outside change to the renamed file still reloads it; a taken name asks or is
refused with a notice), Move to Trash (Cancel keeps the file; on another disk
the item goes to that disk's `.Trash-<uid>`, as the freedesktop rules say).
Checked in the Windows VM with the CI installer (`7f0e9c30`): a change of
case alone (alpha.md to Alpha.md) on an open document, which stays watched;
Move To through the Windows save dialog; the Move to Trash question (Windows
makes Move To Trash its default button, hence the trash for replaced files).
Move to Trash on the VM's shared folder fails with "CanonicalizePath": the
trash crate resolves the path first, which that drive (and Rust's
`canonicalize`) cannot do; the file is left alone. On a local drive (a test
folder in `%TEMP%`, removed afterwards) the item went to the Recycle Bin. Because such
drives cannot resolve paths, the check against copying or moving a folder
into itself now compares the paths as given first (`fileops.rs into_itself`).
Not ported:
copy-with-content-hash for pasted images; stat, unlink and empty-dir, which
the renderer never calls. Deliberate differences from Electron: a file the
user agrees to replace (Rename, Move To) goes to the trash instead of being
overwritten, because the dialog cannot make Cancel its default button as
Electron's does; Paste refuses a name that is taken (fs-extra's copy would
overwrite it silently). Cut and Paste of an open document leaves its tab on
the old path, as in Electron. A reviewer pass found, and the commit fixes: a
link moved onto its target replaced the target's content with a link to
itself (the "same file" test followed links; it now compares the entries
themselves and only allows a change of case); pasting an open file moved the
window's records although the tab stays, so the file could not be opened
again; renaming onto another open file dropped the watch when the renderer
closed the other tab (a path may now appear twice in a window's records); a
change of case alone on Windows lost the watcher's record of the content;
the trash runs on a blocking thread, where a panic in the trash crate's COM
setup comes back as an error.

Find in folder and quick open, part 3 of the sidebar (2026-10-18), checked on
Xvfb: `src-tauri/src/search.rs` stands in for `main/ipc/ripgrep.ts`. Instead
of running the rg binary it uses the crates rg is built from (`ignore` for the
walk, `grep-regex` and `grep-searcher` for matching) with the settings
ripgrep.ts puts on rg's command line, and sends the same `mt::rg::*` events
and payloads; `src/search.ts` handles `mt::rg::start`, `mt::rg::cancel` and
`mt::open-file-by-window-id`. Checked: plain search (9 matches in 5 files,
highlights placed right next to Japanese text and an emoji, which count as
UTF-16 units as in Electron), case sensitive, whole word, regex, a regex
across two lines, clicking a result opens the file, the Include hidden and
Exclusions settings, quick open on Ctrl+P (the shortcut was missing from the
page's command table) opening the chosen file in the same window. As with rg,
a markdown file in the folder's top level is searched even when hidden or
named in an ignore file, because the markdown inclusions win over both;
hidden and ignored folders are still skipped. Differences from Electron: one
thread searches the folders one after another (the renderer only ever passes
one); an invalid regex or max file size ends the search with no results and a
line on stderr, as Electron logs rg's refusal. Upstream renderer quirk: a
match across lines shows a garbled preview, because `searchResultItem.vue`
cuts the text with the start and end columns as if both were on one line.
A reviewer pass compared the events with the real rg binary run through
ripgrep.ts on about 35 cases (encodings, line endings, binary files, globs,
ignore files, links) and found them identical, except that a file whose only
match is an empty one at its end is left out, and columns in invalid UTF-8
now count the replacement characters right. Kept from Electron on purpose: a
multiline regex such as `\n\n` gives each match all the lines from it to the
end of the merged block, so a long file can produce a result of tens of
megabytes (35 MB for an 89 KB file). The commit after the review checks for a
cancel between matches, runs `search_cancel` off the main thread (it waits on
a lock held while an event is sent, which would hang if sending ever needed
the main thread, as it does with Tauri's `tracing` feature), and stops a
search whose id is reused. The CI caught two tests sharing one folder, which
failed on Windows only; each test now has its own. Checked in the Windows VM
with the CI installer (`5398e2ca`): the same 9 matches in 5 files with the
same highlights, case sensitive, opening a result, and quick open from the
command palette (Ctrl+Shift+P, Quick Open; paths shown with backslashes). On
Windows, Ctrl+P prints instead: Electron's Windows table binds both
`file.print` and `file.quick-open` to Ctrl+P, and the first one wins, in
Electron (electron-localshortcut) as in the page. The trial has no print yet,
so WebView2's own print dialog opens. Worth raising upstream, as the command
palette shows Ctrl+P next to Quick Open.

Files dropped onto a window (2026-10-18): `src-tauri/src/drop.rs` and
`src/drop.ts`. The webviews keep their own drag and drop (the editor needs it),
so Tauri's drop handler stays off and the paths come from the webview itself.
Linux: WebKitGTK shows the page no files at all for a drop from outside (types
`text/uri-list` and `text/html`, empty data), so MarkText's import overlay never
appears and WebKit's default action loaded the dropped file in place of the
editor. Rust now watches the view's GTK `drag-data-received` signal, which fires
during the hover without stopping WebKit, and sends the paths to the page,
which takes the drop. Windows: WebView2 gives the page the files but no paths;
the page posts the File objects with `chrome.webview.postMessageWithAdditionalObjects`
and Rust reads each path from the message (`ICoreWebView2File::Path`), so the
renderer's own overlay and `mt::window::drop` work as in Electron. The message
has to be a string (`mt-file-drop:<id>`): WebView2 calls wry's handler for
Tauri's messages first, it fails on any other kind of message, and a failing
handler stops WebView2 calling the ones after it (found in the VM with
logging). wry passes the string on to Tauri, which prints a "JSON error" in
the page's console for each drop; harmless. Handling as
in Electron (`main/menu/actions/file.ts` mt::window::drop): markdown files
open, a file pandoc converts gets the "not available" notice, folders and
other files are ignored. Every window now also refuses to navigate away from
the app's own page, as Electron's `will-navigate` handler does. Checked on
Xvfb with a GTK drag source (`packages/desktop-tauri/scripts/drag-source.py`,
which uses the distrobox's python3-gobject): a markdown file and a folder onto the empty window, a
markdown file and a .docx onto an open document (the editor no longer draws
its drop marker for such a drag, and the document is left unchanged). Checked
in the Windows VM by dragging from Explorer (a local MinGW build, then the CI
installer `c2b5bcbc`): the "Import or Open" overlay, a
markdown file onto it opens in a tab, a .docx gives the notice. As in
Electron, only a drop on the overlay's box counts.

Session restore, the "Restore all" start up action (2026-10-18):
`src-tauri/src/buffers.rs` and `src/buffers.ts` stand in for
`main/editorBufferStore` and `main/windows/editor.ts` `_restoreAllState`. The
renderer already sends each window's whole state (every tab with its unsaved
text, cursor, scroll, folder, layout) through `update-buffer-state`, about a
second after a change and again before the window closes; Rust writes it to
`<app data>/editorStates/<id>_editor_buffer_store.json`, one file per window,
written to a temporary file and renamed. At start up with "Restore all" and
nothing on the command line, each store reopens as a window: saved tabs are
read again from disk, unsaved ones keep their text, untitled tabs come back,
a missing file is reported and its tab kept as unsaved, the folder opens once.
Paths on the command line (or a start up folder) skip restore and delete the
stores whose tabs were all saved. Changes from Electron, decided with the
user: a store that cannot be read is moved aside to `.damaged` and reported;
closing a window without saving deletes its store; quitting keeps every
window's store (Electron deletes a clean window's store whenever other windows
remain, so after a quit only windows with unsaved text plus one came back).
Closing a single window keeps Electron's rule. A reviewer pass led to: a
failed restore can never leave the window unable to save (it starts blank
under a new store and says so); closing asks first whenever the session is not
safely saved; text with a lone UTF-16 surrogate no longer fails every save;
only real damage moves a store aside (a locked file is retried, then left
alone); a page reload restores the window from its own store; writes stop once
a window starts closing. Checked on Xvfb: unsaved and untitled tabs after
quit and restart, a file changed outside and one deleted, two and three
windows (a clean folder window comes back after quit), a command line start,
a damaged store, close without saving under "blank". Not yet seen working: a
page reload (Ctrl+F5 did not reload on Xvfb).

Editor right click menu (2026-10-18): `src/contextMenu.ts` works out the
parameters Electron's `context-menu` event carries (selection, editable,
image, position) from the DOM event and runs Electron's own builder
(`main/contextMenu/editor`) unchanged, through `Menu`/`MenuItem` classes in
`src/electron.ts` and the ☰ menu's popup code (`appMenu.ts popupTemplate`):
Insert Paragraph Before/After, Cut, Copy, Paste, Copy as Rich Text, Copy as
HTML, Paste as Plain Text, with cut and copy greyed out without a selection.
Spelling items stay hidden until spellcheck exists. The webview's own menu is
suppressed in every window, as Electron has none (WebView2's had Back, Refresh,
Print, Inspect); debug builds let Shift + right click through for Inspect.
Checked on Xvfb: the menu at the pointer, Cut, Paste, Insert Paragraph After,
greyed items, no webview menu in the sidebar. Paste from it failed once and not
again in three tries with the same steps. Paste as Plain Text inserts nothing,
from this menu and from the ☰ menu alike: the page reads the clipboard with
`navigator.clipboard.readText` (`backend.ts` mt::clipboard::read-text) and
Muya swallows the error; not yet investigated.

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
| File watching: polling for network drives, `watcherUsePolling` | `src/main/filesystem/watcher.ts` (notify's PollWatcher) |
| Spellcheck | `src/main/spellchecker/`, `src/main/contextMenu/editor/spellcheck.ts` |
| Export to PDF/HTML, print | `src/main/menu/actions/file.ts` (`printToPDF`, `print`) |
| Images: paste, drop, insert from file | `mt::fs::copy-with-content-hash`, `mt::clipboard::*`; a drop needs the path at once (Muya `dragDropImage.ts` calls `getPathForFile`), which only Linux can give, from `src-tauri/src/drop.rs` |
| Missing CLI files as named empty documents | `src/main/filesystem/markdown.ts loadCliMarkdownFile` |
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

### Linux release: AppImage only, at release time

Linux ships as an AppImage and nothing else; Windows as the NSIS installer
CI already builds. Packaging waits until a release is near. Follow the user's
other projects (Sterna is the model, sch-pdf-compare the same; survey of
2026-10-18):
- Build in a release container pinned by digest, separate from the
  development box, with one `packaging/appimage/toolchain.env` for the pinned
  inputs (Sterna `packaging/appimage/toolchain.env`). Add a MarkText case to
  `~/Projects/dev-setup/package-linux.sh` and `projects.json`; `./dev
  package` builds into `~/Projects/Releases/marktext/` and never publishes
  (`dev-setup/collect-appimage.sh`).
- Pinned linuxdeploy and appimagetool versions, `APPIMAGE_EXTRACT_AND_RUN=1`
  in rootless containers, a check that no shipped ELF needs a newer glibc
  than the floor, GLVND frontends only (never Mesa or NVIDIA), and
  `gh-releases-zsync|nataloko|marktext|latest|marktext-x86_64.AppImage.zsync`
  update information checked with `readelf --string-dump=.upd_info`.
- Release flow: tag pushes build a draft GitHub release; signing stays
  offline (Sterna `packaging/RELEASING.md`, `packaging/release.sh`).
- Where MarkText differs: the other projects build on manylinux_2_28
  (glibc 2.28), which has no WebKitGTK 4.1 or libsoup3. Choose the base first;
  Ubuntu 22.04 (glibc 2.35) is the likely floor. Bundling WebKitGTK (its
  WebProcess and NetworkProcess helpers) makes the image large and is where
  AppImages of Tauri apps usually break on other distros (EGL and graphics
  drivers), so do one throwaway CI build and test on Fedora and Ubuntu before
  release week. Try `tauri build --bundles appimage` in the container with
  linuxdeploy pre-seeded at the pinned version; fall back to a Sterna style
  AppRun only if needed. Trim `bundle.targets` in `src-tauri/tauri.conf.json`
  to `appimage` and `nsis`.

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

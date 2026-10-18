// Answers the messages the renderer normally sends to Electron's main process
// for this window. Channel names and payloads follow packages/desktop/src/main;
// the file:line notes point at the Electron code each handler stands in for.
// Preferences, files, window controls and the ☰ menu live in their own modules.
// Anything not handled is logged once by ./electron.

import { invoke } from '@tauri-apps/api/core'
import { ask, open as openDialog } from '@tauri-apps/plugin-dialog'
import { openPath, openUrl, revealItemInDir } from '@tauri-apps/plugin-opener'
import type { MenuPopupPosition, MenuTemplate } from '@shared/types/menu'
import { registerMenuHandlers } from './appMenu'
import { appWindow, boot, isSettings, isWindows } from './boot'
import {
  allowWrites,
  cancelClose,
  closeBuffer,
  closeRisk,
  restoreSession,
  updateBufferState
} from './buffers'
import { listenForDrops, registerDrop } from './drop'
import { emitToRenderer, onInvoke, onSend, onSendSync, whenListening } from './electron'
import {
  listenForFileChanges,
  openTab,
  openTabs,
  saveTab,
  showOpenDialog,
  type SaveRequest
} from './files'
import { installContextMenu } from './contextMenu'
import { registerFileOperations } from './fileOps'
import { listenForFolder, openFolder, showOpenFolderDialog } from './folders'
import { installLocalImages } from './localImages'
import { popupMenu } from './menu'
import { installPasteGuard } from './paste'
import {
  forRenderer,
  listenForPreferences,
  loadPreferences,
  preferences,
  preferredEol,
  setPreferences
} from './preferences'
import { registerSearch } from './search'
import {
  defaultKeybindings,
  ensureKeybindingsFile,
  keybindings,
  saveUserKeybindings,
  sendKeybindings,
  userKeybindings
} from './keybindings'
import { installShortcuts } from './shortcuts'
import { listenForUserData, registerUserData } from './userData'
import {
  closeWindow,
  fullscreen,
  installWindowEvents,
  newWindow,
  openSettings,
  quitApp,
  setFullscreen
} from './window'

const translations = import.meta.glob('../../desktop/static/locales/*.json', { import: 'default' })

// The renderer reads its first settings from the page URL
// (packages/desktop/src/main/windows/base.ts _buildUrlWithSettings).
const setWindowUrlParams = (): void => {
  const params = new URLSearchParams({
    udp: boot.paths.userData,
    debug: import.meta.env.DEV ? '1' : '0',
    wid: String(boot.windowId),
    // The router shows /preference for "settings", /spelling for "settings/spelling".
    type: isSettings ? (boot.category ? `settings/${boot.category}` : 'settings') : 'editor',
    cff: String(preferences.codeFontFamily ?? ''),
    cfs: String(preferences.codeFontSize),
    hsb: preferences.hideScrollbar ? '1' : '0',
    theme: String(preferences.theme ?? ''),
    tbs: 'custom'
  })
  history.replaceState(null, '', `${location.pathname}?${params}${location.hash}`)
}

// ---- commands from shortcuts -----------------------------------------------

const FORMAT_TYPES: Record<string, string> = {
  'format.strong': 'strong',
  'format.emphasis': 'em',
  'format.underline': 'u',
  'format.superscript': 'sup',
  'format.subscript': 'sub',
  'format.highlight': 'mark',
  'format.inline-code': 'inline_code',
  'format.inline-math': 'inline_math',
  'format.strike': 'del',
  'format.hyperlink': 'link',
  'format.image': 'image',
  'format.clear-format': 'clear'
}

const PARAGRAPH_TYPES: Record<string, string> = {
  'paragraph.heading-1': 'heading 1',
  'paragraph.heading-2': 'heading 2',
  'paragraph.heading-3': 'heading 3',
  'paragraph.heading-4': 'heading 4',
  'paragraph.heading-5': 'heading 5',
  'paragraph.heading-6': 'heading 6',
  'paragraph.upgrade-heading': 'upgrade heading',
  'paragraph.degrade-heading': 'degrade heading',
  'paragraph.table': 'table',
  'paragraph.code-fence': 'pre',
  'paragraph.quote-block': 'blockquote',
  'paragraph.math-formula': 'mathblock',
  'paragraph.html-block': 'html',
  'paragraph.order-list': 'ol-order',
  'paragraph.bullet-list': 'ul-bullet',
  'paragraph.task-list': 'ul-task',
  'paragraph.loose-list-item': 'loose-list-item',
  'paragraph.paragraph': 'paragraph',
  'paragraph.horizontal-line': 'hr',
  'paragraph.front-matter': 'front-matter'
}

// Cut, copy and paste are left to the webview, as Electron's
// webContents.cut/copy/paste do (main/menu/actions/edit.ts).
const EDIT_TYPES: Record<string, string> = {
  'edit.undo': 'undo',
  'edit.redo': 'redo',
  'edit.copy-as-rich': 'copyAsRich',
  'edit.copy-as-html': 'copyAsHtml',
  'edit.paste-as-plaintext': 'pasteAsPlainText',
  'edit.select-all': 'selectAll',
  'edit.duplicate': 'duplicate',
  'edit.create-paragraph': 'createParagraph',
  'edit.delete-paragraph': 'deleteParagraph',
  'edit.find': 'find',
  'edit.find-next': 'findNext',
  'edit.find-previous': 'findPrev',
  'edit.replace': 'replace'
}

const TAB_INDEX = [
  'First',
  'Second',
  'Third',
  'Fourth',
  'Fifth',
  'Sixth',
  'Seventh',
  'Eighth',
  'Ninth',
  'Tenth'
]

// Returns false for commands the trial does not support, so the key press
// reaches the page untouched.
const runCommand = (id: string): boolean => {
  if (FORMAT_TYPES[id]) emitToRenderer('mt::editor-format-action', { type: FORMAT_TYPES[id] })
  else if (PARAGRAPH_TYPES[id])
    emitToRenderer('mt::editor-paragraph-action', { type: PARAGRAPH_TYPES[id] })
  else if (EDIT_TYPES[id]) emitToRenderer('mt::editor-edit-action', EDIT_TYPES[id])
  else if (id.startsWith('tabs.switchTo') && TAB_INDEX.includes(id.slice(13))) {
    emitToRenderer('mt::switch-tab-by-index', TAB_INDEX.indexOf(id.slice(13)))
  } else {
    switch (id) {
      case 'file.new-window':
        void newWindow()
        break
      case 'file.preferences':
        void openSettings()
        break
      case 'file.new-tab':
        emitToRenderer('mt::new-untitled-tab')
        emitToRenderer('mt::set-view-layout', { showTabBar: true })
        break
      case 'file.open-file':
        void showOpenDialog()
        break
      case 'file.open-folder':
        void showOpenFolderDialog()
        break
      case 'file.quick-open':
        emitToRenderer('mt::execute-command-by-id', 'file.quick-open')
        break
      case 'file.save':
        emitToRenderer('mt::editor-ask-file-save')
        break
      case 'file.save-as':
        emitToRenderer('mt::editor-ask-file-save-as')
        break
      case 'file.close-tab':
        emitToRenderer('mt::editor-close-tab')
        break
      case 'file.close-window':
        emitToRenderer('mt::ask-for-close')
        break
      case 'file.quit':
        void quitApp()
        break
      case 'window.minimize':
        void appWindow.minimize()
        break
      case 'window.toggle-full-screen':
        void setFullscreen(!fullscreen())
        break
      case 'view.command-palette':
        emitToRenderer('mt::show-command-palette')
        break
      case 'view.source-code-mode':
        emitToRenderer('mt::toggle-view-mode-entry', 'sourceCode')
        break
      case 'view.typewriter-mode':
        emitToRenderer('mt::toggle-view-mode-entry', 'typewriter')
        break
      case 'view.focus-mode':
        emitToRenderer('mt::toggle-view-mode-entry', 'focus')
        break
      case 'view.toggle-sidebar':
        emitToRenderer('mt::toggle-view-layout-entry', 'showSideBar')
        break
      case 'view.toggle-tabbar':
        emitToRenderer('mt::toggle-view-layout-entry', 'showTabBar')
        break
      case 'view.toggle-dev-tools':
        void invoke('toggle_devtools')
        break
      case 'view.dev-reload':
        location.reload()
        break
      case 'tabs.cycleForward':
      case 'tabs.switchToRight':
        emitToRenderer('mt::tabs-cycle-right')
        break
      case 'tabs.cycleBackward':
      case 'tabs.switchToLeft':
        emitToRenderer('mt::tabs-cycle-left')
        break
      default:
        return false
    }
  }
  return true
}

// ---- renderer context menus --------------------------------------------------

const menuPrefix = `${appWindow.label}:`

// Renderer context menus (contextMenu/popupMenu.ts) wait for a click and then
// for mt::menu::closed. GTK reports no dismissal without a pick, so closed
// only follows a click; a dismissed popup's listeners stay registered but
// never match the ids of a later popup.
const showRendererMenu = (template: MenuTemplate, position?: MenuPopupPosition) =>
  popupMenu(template, position, menuPrefix, (id) => {
    emitToRenderer('mt::menu::click', { windowId: boot.windowId, id })
    emitToRenderer('mt::menu::closed', { windowId: boot.windowId })
  })

// ---- channel handlers ------------------------------------------------------

const registerHandlers = (): void => {
  onSendSync('mt::boot-info', () => boot)
  // Only reached when the two paths differ by case (preload isSamePathSync).
  onSendSync(
    'mt::paths::is-same-sync',
    (a: string, b: string) => isWindows && a.toLowerCase() === b.toLowerCase()
  )

  onSend('mt::ask-for-user-preference', () =>
    emitToRenderer('mt::user-preference', { ...forRenderer(preferences), titleBarStyle: 'custom' })
  )
  onSend('mt::get-current-language', () =>
    emitToRenderer('mt::current-language', preferences.language || 'en')
  )
  onSend('mt::set-user-preference', setPreferences)
  onSend('mt::cmd-toggle-autosave', () => setPreferences({ autoSave: !preferences.autoSave }))
  onSend('mt::request-keybindings', sendKeybindings)

  onSend('mt::cmd-open-file', showOpenDialog)
  onSend('mt::cmd-open-folder', () => showOpenFolderDialog())
  onSend('mt::ask-for-open-project-in-sidebar', () => showOpenFolderDialog(true))
  onSend('mt::open-file', (pathname: string) => openTab(pathname))
  onSend('mt::window-tab-closed', (pathname: string) =>
    invoke('window_remove_file', { path: pathname })
  )
  onSend('mt::cmd-new-editor-window', newWindow)
  onSend('mt::open-setting-window', (category?: string) => openSettings(category))
  onSend('mt::response-file-save', (...args: SaveRequest) => saveTab(false, ...args))
  onSend('mt::response-file-save-as', (...args: SaveRequest) => saveTab(true, ...args))

  // Settings has no unsaved documents and closes at once (windows/setting.ts).
  onSend('mt::win::close', () => (isSettings ? closeWindow() : emitToRenderer('mt::ask-for-close')))
  // The renderer saved the session just before (store/editor.ts
  // LISTEN_FOR_CLOSE). With "Restore all" it closes a window with unsaved
  // changes without asking, so a session that is not saved must not go
  // unnoticed.
  onSend('mt::close-window', async() => {
    const risk = await closeRisk()
    if (risk) {
      const closeAnyway = await ask(
        `${risk} Unsaved changes in this window may be lost. Close anyway?`,
        { title: 'MarkText', kind: 'warning' }
      )
      if (!closeAnyway) return cancelClose()
    }
    await closeBuffer(false)
    await closeWindow()
  })
  onSend('mt::close-window-confirm', async(unsavedFiles: unknown[]) => {
    const discard = await ask(
      `${unsavedFiles.length} file(s) have unsaved changes. Close without saving?`,
      { title: 'MarkText', kind: 'warning' }
    )
    if (!discard) return cancelClose()
    await closeBuffer(true)
    await closeWindow()
  })
  onSend('mt::win::minimize', () => appWindow.minimize())
  onSend('mt::win::maximize', () => appWindow.maximize())
  onSend('mt::win::unmaximize', () => appWindow.unmaximize())
  onSend('mt::win::toggle-maximize', () => appWindow.toggleMaximize())
  onSend('mt::win::set-fullscreen', (flag: boolean) => setFullscreen(flag))
  onSend('mt::win::toggle-fullscreen', () => setFullscreen(!fullscreen()))
  onInvoke('mt::win::is-maximized', () => appWindow.isMaximized())
  onInvoke('mt::win::is-fullscreen', () => appWindow.isFullscreen())
  onSend('mt::menu::popup', showRendererMenu)
  registerMenuHandlers()

  onSend('mt::shell::show-item', (fullPath: string) => revealItemInDir(fullPath))
  onInvoke('mt::shell::open-external', (url: string) => openUrl(url))
  onInvoke('mt::shell::open-path', (fullPath: string) => openPath(fullPath))
  onSend('mt::clipboard::write-text', (text: string) => invoke('clipboard_write_text', { text }))
  onInvoke('mt::clipboard::read-text', () => invoke<string>('clipboard_read_text'))
  // A Uint8Array would reach Rust as a JSON object, not a list of bytes.
  onInvoke('mt::clipboard::write-image', (png: Uint8Array) =>
    invoke<boolean>('clipboard_write_image', { png: Array.from(png) })
  )
  onInvoke('mt::clipboard::guess-file-path', () => '')
  onInvoke('mt::fs::path-exists', (p: string) => invoke('path_exists', { path: p }))
  registerFileOperations()
  registerUserData()
  registerSearch()
  registerDrop()

  onInvoke('mt::i18n::load', async(language: string) => {
    const load = translations[`../../desktop/static/locales/${language}.json`]
    return load ? load() : null
  })

  // ---- settings pages ----
  // index.ts mt::select-default-directory-to-open.
  onSend('mt::select-default-directory-to-open', async() => {
    const directory = await openDialog({ directory: true })
    if (typeof directory === 'string') await setPreferences({ defaultDirectoryToOpen: directory })
  })
  // No keymap makes the Keybindings page fall back to the US layout.
  onInvoke('mt::keybinding-get-keyboard-info', () => ({ layout: null, keymap: null }))
  onInvoke('mt::keybinding-get-pref-keybindings', () => ({
    defaultKeybindings: new Map(defaultKeybindings(boot.platform)),
    userKeybindings: new Map(userKeybindings)
  }))
  onInvoke('mt::keybinding-save-user-keybindings', saveUserKeybindings)
  onSend('mt::open-keybindings-config', async() => {
    try {
      await openPath(await ensureKeybindingsFile())
    } catch (err) {
      console.error('[keybindings] could not open keybindings.json:', err)
    }
  })
  // There is no native keyboard layout to dump here.
  onSend('mt::keybinding-debug-dump-keyboard-info', () =>
    console.info('Keyboard layout information is not available in the Tauri build.')
  )
  // Chromium's spellchecker dictionaries have no counterpart in the webviews;
  // empty answers keep the Spelling page from throwing.
  onInvoke('mt::spellchecker-get-available-dictionaries', () => [])
  onInvoke('mt::spellchecker-get-custom-dictionary-words', () => [])
  onInvoke('mt::spellchecker-switch-language', () => null)
  onInvoke('mt::spellchecker-remove-word', () => false)

  onInvoke('update-buffer-state', updateBufferState)

  onSend('mt::handle-renderer-error', (...args: unknown[]) =>
    console.error('[renderer error]', ...args)
  )

  // Bookkeeping that has no counterpart in the trial.
  onSend('mt::window-initialized', () => {})
}

// Files a second launch sends to this window (src-tauri/src/windows.rs). They
// open after the window's own files, in the order they arrive.
let opening: Promise<void> = Promise.resolve()

// Messages Rust addresses to this window's renderer.
const listenForRendererMessages = (): Promise<unknown> =>
  appWindow.listen<{ channel: string; args: unknown[] }>(
    'mt::tauri::renderer-message',
    ({ payload }) => emitToRenderer(payload.channel, ...payload.args)
  )

const listenForFiles = (): Promise<unknown> =>
  appWindow.listen<string[]>('mt::tauri::open-files', ({ payload }) => {
    opening = opening.then(() => openTabs(payload))
  })

const bootstrap = (addBlankTab: boolean): void => {
  emitToRenderer('mt::bootstrap-editor', {
    addBlankTab,
    markdownList: [],
    lineEnding: preferredEol(),
    sideBarVisibility: preferences.restoreLayoutState ? !!preferences.sideBarVisibility : false,
    tabBarVisibility: !!preferences.tabBarVisibility,
    sourceCodeModeEnabled: !!preferences.sourceCodeModeEnabled
  })
}

const bootEditor = async(): Promise<void> => {
  // Files routed here while the window was loading waited in Rust until now.
  const ready = await invoke<{ files: string[]; booted: boolean }>('window_ready')
  // A reloaded page brings back its own session, which holds its files and folder.
  const restore = boot.restored || ready.booted
  const files = restore ? ready.files : [...boot.openFiles, ...ready.files]
  await whenListening('mt::bootstrap-editor')
  if (restore) {
    await whenListening('mt::open-directory')
    // The session replaces every tab, so routed files open after it.
    await restoreSession(bootstrap)
  } else {
    allowWrites()
    bootstrap(files.length === 0 && !boot.openFolder)
  }
  if (boot.openFolder && !restore) {
    await whenListening('mt::open-directory')
    await openFolder(boot.openFolder, true)
  }
  if (!files.length) return
  await whenListening('mt::open-new-tab')
  await openTabs(files)
}

// Must finish before the preload runs, because the preload reads boot info
// synchronously and the renderer reads its settings from the URL.
export const startBackend = async(): Promise<void> => {
  registerHandlers()
  await listenForPreferences()
  await listenForUserData()
  await loadPreferences()
  setWindowUrlParams()
  await installWindowEvents()
  installContextMenu()
  installPasteGuard()
  installLocalImages()
  await listenForRendererMessages()
  // The settings window has no documents, menu or editor shortcuts.
  if (isSettings) return
  installShortcuts(keybindings, runCommand)
  await listenForFileChanges()
  await listenForFolder()
  await listenForFiles()
  await listenForDrops()
  opening = bootEditor()
}

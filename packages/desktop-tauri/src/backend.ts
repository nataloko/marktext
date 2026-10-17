// Answers the messages the renderer normally sends to Electron's main process,
// for a single editor window: boot, preferences, open, save, close and window
// controls. Channel names and payloads follow packages/desktop/src/main; the
// file:line notes point at the Electron code each handler stands in for.
// Anything not handled here is logged once by ./electron.

import { invoke } from '@tauri-apps/api/core'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { ask, open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog'
import { openPath, openUrl, revealItemInDir } from '@tauri-apps/plugin-opener'
import { createPlatformPath } from 'common/filesystem/platformPath'
import type { BootInfo } from '@shared/types/ipc'
import defaultPreferences from '../../desktop/static/preference.json'
import { emitToRenderer, onInvoke, onSend, onSendSync, whenListening } from './electron'
import { loadMarkdownFile, writeMarkdownFile, type MarkdownOptions } from './markdown'
import { defaultKeybindings, installShortcuts } from './shortcuts'

interface TauriBootInfo extends BootInfo {
  // Absolute paths given on the command line.
  openFiles: string[]
}

declare global {
  interface Window {
    __MT_BOOT__: TauriBootInfo
  }
}

const boot = window.__MT_BOOT__
const isWindows = boot.platform === 'win32'
const path = createPlatformPath(isWindows)
const appWindow = getCurrentWindow()
const keybindings = defaultKeybindings(boot.platform)

const MARKDOWN_EXTENSIONS = [
  'md',
  'markdown',
  'mdown',
  'mkdn',
  'mkd',
  'mdwn',
  'mdtxt',
  'mdtext',
  'mdx',
  'txt'
]

const translations = import.meta.glob('../../desktop/static/locales/*.json', { import: 'default' })

// ---- preferences -----------------------------------------------------------

type Preferences = Record<string, unknown>

const preferencesPath = path.join(boot.paths.userData, 'preferences.json')
let preferences: Preferences = { ...defaultPreferences }

const loadPreferences = async(): Promise<void> => {
  let stored: Preferences = {}
  try {
    stored = JSON.parse(await invoke<string>('read_text_file', { path: preferencesPath }))
  } catch {
    // First run (packages/desktop/src/main/preferences/index.ts:65).
    if (window.matchMedia('(prefers-color-scheme: dark)').matches) stored.theme = 'dark'
  }
  preferences = { ...defaultPreferences, ...stored }
}

const savePreferences = (): Promise<unknown> =>
  invoke('write_text_file', {
    path: preferencesPath,
    contents: JSON.stringify(preferences, null, 2)
  })

// What the renderer sees. Session restore is not implemented in the trial, so
// "restoreAll" would let the window close without asking about unsaved tabs
// (store/editor.ts LISTEN_FOR_CLOSE).
const rendererPreferences = (): Preferences => ({
  ...preferences,
  startUpAction: preferences.startUpAction === 'restoreAll' ? 'blank' : preferences.startUpAction,
  titleBarStyle: 'custom'
})

const preferredEol = (): 'lf' | 'crlf' => {
  const endOfLine = preferences.endOfLine
  if (endOfLine === 'lf') return 'lf'
  return endOfLine === 'crlf' || isWindows ? 'crlf' : 'lf'
}

// The renderer reads its first settings from the page URL
// (packages/desktop/src/main/windows/base.ts _buildUrlWithSettings).
const setWindowUrlParams = (): void => {
  const params = new URLSearchParams({
    udp: boot.paths.userData,
    debug: import.meta.env.DEV ? '1' : '0',
    wid: '1',
    type: 'editor',
    cff: String(preferences.codeFontFamily ?? ''),
    cfs: String(preferences.codeFontSize),
    hsb: preferences.hideScrollbar ? '1' : '0',
    theme: String(preferences.theme ?? ''),
    tbs: 'custom'
  })
  history.replaceState(null, '', `${location.pathname}?${params}${location.hash}`)
}

// ---- files -----------------------------------------------------------------

const openTab = async(pathname: string, selected = true): Promise<void> => {
  try {
    const document = await loadMarkdownFile(pathname, {
      preferredEol: preferredEol(),
      trimTrailingNewline: Number(preferences.trimTrailingNewline ?? 2),
      autoNormalizeLineEndings: !!preferences.autoNormalizeLineEndings
    })
    emitToRenderer('mt::open-new-tab', document, {}, selected)
  } catch (err) {
    emitToRenderer('mt::show-notification', {
      title: 'Cannot open tab',
      type: 'error',
      message: String(err)
    })
  }
}

const showOpenDialog = async(): Promise<void> => {
  const picked = await openDialog({
    multiple: true,
    filters: [{ name: 'Markdown', extensions: MARKDOWN_EXTENSIONS }]
  })
  const paths = picked === null ? [] : Array.isArray(picked) ? picked : [picked]
  for (const [index, pathname] of paths.entries()) {
    await openTab(pathname, index === 0)
  }
}

// Arguments of mt::response-file-save and mt::response-file-save-as.
type SaveRequest = [
  id: string,
  filename: string,
  pathname: string,
  markdown: string,
  options: MarkdownOptions,
  defaultPath?: string
]

// packages/desktop/src/main/menu/actions/file.ts handleResponseForSave and Save As.
const saveTab = async(
  forceDialog: boolean,
  ...[id, filename, pathname, markdown, options, defaultPath]: SaveRequest
): Promise<void> => {
  const existedOnDisk = !!pathname && (await invoke<boolean>('path_exists', { path: pathname }))
  let target = pathname

  if (forceDialog || !pathname) {
    const name = /\.[a-z0-9]+$/i.test(filename) ? filename : `${filename || 'Untitled'}.md`
    const directory = pathname ? path.dirname(pathname) : defaultPath || boot.env.HOME || ''
    const chosen = await saveDialog({
      defaultPath: directory ? path.join(directory, name) : name,
      filters: [{ name: 'Markdown', extensions: ['md', 'markdown'] }]
    })
    if (!chosen) return
    target = chosen
  }

  try {
    await writeMarkdownFile(target, markdown, options)
    if (target !== pathname || !existedOnDisk) {
      emitToRenderer('mt::set-pathname', { id, pathname: target, filename: path.basename(target) })
    } else {
      emitToRenderer('mt::tab-saved', id)
    }
  } catch (err) {
    emitToRenderer('mt::tab-save-failure', id, String(err))
  }
}

// ---- window ----------------------------------------------------------------

let isFullscreen = false

const setFullscreen = async(flag: boolean): Promise<void> => {
  await appWindow.setFullscreen(flag)
  isFullscreen = flag
  emitToRenderer(flag ? 'mt::window-enter-full-screen' : 'mt::window-leave-full-screen')
}

const closeWindow = async(): Promise<void> => {
  await appWindow.destroy()
}

const installWindowEvents = async(): Promise<void> => {
  let wasMaximized = await appWindow.isMaximized()
  await appWindow.onResized(async() => {
    const maximized = await appWindow.isMaximized()
    if (maximized === wasMaximized) return
    wasMaximized = maximized
    emitToRenderer(maximized ? 'mt::window-maximize' : 'mt::window-unmaximize')
  })
  await appWindow.onFocusChanged(({ payload }) => {
    emitToRenderer('mt::window-active-status', { status: payload })
  })
  // The window manager's close (Alt+F4) asks the renderer about unsaved tabs first.
  await appWindow.onCloseRequested((event) => {
    event.preventDefault()
    emitToRenderer('mt::ask-for-close')
  })

  // Electron copies document.title into the window title; Tauri does not.
  const title = document.querySelector('title')
  if (title) {
    new MutationObserver(() => appWindow.setTitle(document.title)).observe(title, {
      childList: true,
      characterData: true,
      subtree: true
    })
  }

  // Replaces `-webkit-app-region: drag` on the custom title bar
  // (components/titleBar/index.vue), which only Chromium understands.
  const noDrag = '.title-no-drag, .frameless-titlebar-button, .word-count, button, input, a'
  window.addEventListener('mousedown', (event) => {
    const target = event.target as Element | null
    if (event.button !== 0 || !target?.closest('.title-bar') || target.closest(noDrag)) return
    if (event.detail === 2) appWindow.toggleMaximize()
    else appWindow.startDragging()
  })
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
      case 'file.new-tab':
        emitToRenderer('mt::new-untitled-tab')
        emitToRenderer('mt::set-view-layout', { showTabBar: true })
        break
      case 'file.open-file':
        void showOpenDialog()
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
      case 'file.quit':
        emitToRenderer('mt::ask-for-close')
        break
      case 'window.minimize':
        void appWindow.minimize()
        break
      case 'window.toggle-full-screen':
        void setFullscreen(!isFullscreen)
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

// ---- channel handlers ------------------------------------------------------

const registerHandlers = (): void => {
  onSendSync('mt::boot-info', () => boot)
  // Only reached when the two paths differ by case (preload isSamePathSync).
  onSendSync(
    'mt::paths::is-same-sync',
    (a: string, b: string) => isWindows && a.toLowerCase() === b.toLowerCase()
  )

  onSend('mt::ask-for-user-preference', () =>
    emitToRenderer('mt::user-preference', rendererPreferences())
  )
  onSend('mt::ask-for-user-data', () => emitToRenderer('mt::user-preference', {}))
  onSend('mt::get-current-language', () =>
    emitToRenderer('mt::current-language', preferences.language || 'en')
  )
  onSend('mt::set-user-preference', async(settings: Preferences) => {
    preferences = { ...preferences, ...settings }
    await savePreferences()
    emitToRenderer('mt::user-preference', settings)
  })
  onSend('mt::request-keybindings', () =>
    emitToRenderer('mt::keybindings-response', Object.fromEntries(keybindings))
  )

  onSend('mt::cmd-open-file', showOpenDialog)
  onSend('mt::response-file-save', (...args: SaveRequest) => saveTab(false, ...args))
  onSend('mt::response-file-save-as', (...args: SaveRequest) => saveTab(true, ...args))

  onSend('mt::win::close', () => emitToRenderer('mt::ask-for-close'))
  onSend('mt::close-window', closeWindow)
  onSend('mt::close-window-confirm', async(unsavedFiles: unknown[]) => {
    const discard = await ask(
      `${unsavedFiles.length} file(s) have unsaved changes. Close without saving?`,
      { title: 'MarkText', kind: 'warning' }
    )
    if (discard) await closeWindow()
  })
  onSend('mt::win::minimize', () => appWindow.minimize())
  onSend('mt::win::maximize', () => appWindow.maximize())
  onSend('mt::win::unmaximize', () => appWindow.unmaximize())
  onSend('mt::win::toggle-maximize', () => appWindow.toggleMaximize())
  onSend('mt::win::set-fullscreen', (flag: boolean) => setFullscreen(flag))
  onSend('mt::win::toggle-fullscreen', () => setFullscreen(!isFullscreen))
  onInvoke('mt::win::is-maximized', () => appWindow.isMaximized())
  onInvoke('mt::win::is-fullscreen', () => appWindow.isFullscreen())

  onSend('mt::shell::show-item', (fullPath: string) => revealItemInDir(fullPath))
  onInvoke('mt::shell::open-external', (url: string) => openUrl(url))
  onInvoke('mt::shell::open-path', (fullPath: string) => openPath(fullPath))
  onSend('mt::clipboard::write-text', (text: string) => navigator.clipboard.writeText(text))
  onInvoke('mt::clipboard::read-text', () => navigator.clipboard.readText())
  onInvoke('mt::clipboard::guess-file-path', () => '')
  onInvoke('mt::fs::path-exists', (p: string) => invoke('path_exists', { path: p }))

  onInvoke('mt::i18n::load', async(language: string) => {
    const load = translations[`../../desktop/static/locales/${language}.json`]
    return load ? load() : null
  })

  // Session restore is out of scope; the renderer only needs a resolved promise.
  onInvoke('update-buffer-state', () => true)

  onSend('mt::handle-renderer-error', (...args: unknown[]) =>
    console.error('[renderer error]', ...args)
  )

  // Menu and session bookkeeping that has no counterpart in the trial.
  for (const channel of [
    'mt::window-initialized',
    'mt::view-layout-changed',
    'mt::update-line-ending-menu',
    'mt::set-editor-format-menus-enabled',
    'mt::update-format-menu',
    'mt::ask-for-open-project-in-sidebar'
  ]) {
    onSend(channel, () => {})
  }
}

const bootEditor = async(): Promise<void> => {
  await whenListening('mt::bootstrap-editor')
  emitToRenderer('mt::bootstrap-editor', {
    addBlankTab: boot.openFiles.length === 0,
    markdownList: [],
    lineEnding: preferredEol(),
    sideBarVisibility: preferences.restoreLayoutState ? !!preferences.sideBarVisibility : false,
    tabBarVisibility: !!preferences.tabBarVisibility,
    sourceCodeModeEnabled: !!preferences.sourceCodeModeEnabled
  })
  if (!boot.openFiles.length) return
  await whenListening('mt::open-new-tab')
  for (const [index, pathname] of boot.openFiles.entries()) {
    await openTab(pathname, index === 0)
  }
}

// Must finish before the preload runs, because the preload reads boot info
// synchronously and the renderer reads its settings from the URL.
export const startBackend = async(): Promise<void> => {
  registerHandlers()
  await loadPreferences()
  setWindowUrlParams()
  installShortcuts(keybindings, runCommand)
  await installWindowEvents()
  void bootEditor()
}

// The ☰ application menu, built from the Electron menu templates
// (packages/desktop/src/main/menu/templates) as Electron's AppMenu does
// (packages/desktop/src/main/menu/index.ts). It is rebuilt each time it opens,
// so labels, preferences and recent files are current, and the renderer's
// latest menu updates (check marks, greyed out items) are replayed onto it in
// the order they arrived. The templates and most actions run unchanged; the
// main process modules they import that cannot run in a page resolve to
// ./stand-ins (see vite.config.ts).

import { invoke } from '@tauri-apps/api/core'
import type {
  BaseWindow,
  Menu as ElectronMenu,
  MenuItem as ElectronMenuItem,
  MenuItemConstructorOptions
} from 'electron'
import type { MenuPopupPosition } from '@shared/types/menu'
import buildEditorMenu from '../../desktop/src/main/menu/templates'
import { updateSidebarMenu } from '../../desktop/src/main/menu/actions/edit'
import { updateFormatMenu } from '../../desktop/src/main/menu/actions/format'
import {
  updateSelectionMenus,
  type SelectionState
} from '../../desktop/src/main/menu/actions/paragraph'
import { viewLayoutChanged } from '../../desktop/src/main/menu/actions/view'
import type Keybindings from '../../desktop/src/main/keyboard/shortcutHandler'
import type Preference from '../../desktop/src/main/preferences'
import { appWindow, boot } from './boot'
import { emitToRenderer, ipcMain, onSend } from './electron'
import { showPopup, type ItemOptions } from './menu'
import { preferences, setPreferences } from './preferences'
import { defaultKeybindings } from './shortcuts'
import { alwaysOnTop, fullscreen, setFullscreen, toggleAlwaysOnTop } from './window'

// What the menu actions receive as Electron's BrowserWindow.
export interface MenuWindow {
  id: number
  webContents: {
    send(channel: string, ...args: unknown[]): void
    getZoomFactor(): number
    toggleDevTools(): void
  }
  minimize(): void
  setFullScreen(flag: boolean): void
  isFullScreen(): boolean
  close(): void
}

const menuWindow: MenuWindow = {
  id: boot.windowId,
  webContents: {
    send: (channel, ...args) => emitToRenderer(channel, ...args),
    // main/windows/utils.ts zooms from this; the renderer saves the new zoom.
    getZoomFactor: () => Number(preferences.zoom ?? 1),
    toggleDevTools: () => void invoke('toggle_devtools')
  },
  minimize: () => void appWindow.minimize(),
  setFullScreen: (flag) => void setFullscreen(flag),
  isFullScreen: fullscreen,
  close: () => emitToRenderer('mt::ask-for-close')
}

const keybindingTable = defaultKeybindings(boot.platform)
const keybindings = {
  getAccelerator: (id: string) => keybindingTable.get(id) || null
} as unknown as Keybindings

const preferenceStore = {
  getAll: () => preferences,
  getItem: (key: string) => preferences[key]
} as unknown as Preference

// ---- menu model ---------------------------------------------------------------

// The slice of Electron's Menu and MenuItem that the templates' click handlers
// and the update helpers in main/menu/actions use.
interface ModelItem {
  id?: string
  label?: string
  type?: string
  role?: string
  accelerator?: string
  checked: boolean
  enabled: boolean
  visible: boolean
  click?: MenuItemConstructorOptions['click']
  submenu?: ModelMenu
}

class ModelMenu {
  constructor(public items: ModelItem[]) {}

  getMenuItemById(id: string): ModelItem | null {
    for (const item of this.items) {
      if (item.id === id) return item
      const found = item.submenu?.getMenuItemById(id)
      if (found) return found
    }
    return null
  }
}

const toModel = (template: MenuItemConstructorOptions[]): ModelMenu =>
  new ModelMenu(
    template.map((options) => ({
      id: options.id,
      label: options.label,
      type: options.type,
      role: options.role,
      accelerator: options.accelerator as string | undefined,
      checked: !!options.checked,
      enabled: options.enabled !== false,
      visible: options.visible !== false,
      click: options.click,
      submenu: Array.isArray(options.submenu) ? toModel(options.submenu) : undefined
    }))
  )

// ---- updates from the renderer ------------------------------------------------

type Update = (menu: ElectronMenu) => void

// The latest update of each kind, oldest first.
const updates = new Map<string, Update>()
let viewLayout: Record<string, unknown> = {}

const record = (kind: string, update: Update): void => {
  updates.delete(kind)
  updates.set(kind, update)
}

const setLineEnding = (menu: ElectronMenu, lineEnding: string): void => {
  const id = lineEnding === 'crlf' ? 'crlfLineEndingMenuEntry' : 'lfLineEndingMenuEntry'
  const item = menu.getMenuItemById(id)
  if (item) item.checked = true
}

// In source code mode the Paragraph and Format commands would act on the
// hidden editor, so they are greyed out (main/menu/index.ts, #3531).
const setFormatMenusEnabled = (menu: ElectronMenu, enabled: boolean): void => {
  for (const id of ['paragraphMenuEntry', 'formatMenuItem']) {
    menu.getMenuItemById(id)?.submenu?.items.forEach((item) => (item.enabled = enabled))
  }
}

const setAlwaysOnTop = (menu: ElectronMenu, flag: boolean): void => {
  const item = menu.getMenuItemById('alwaysOnTopMenuItem')
  if (item) item.checked = flag
}

// ---- showing it -----------------------------------------------------------------

// Cut, copy and paste have no click handler in the templates; Electron runs
// them on the focused page.
const ROLES: Record<string, 'Cut' | 'Copy' | 'Paste'> = {
  cut: 'Cut',
  copy: 'Copy',
  paste: 'Paste'
}

// Tauri rejects Electron's "Plus" and characters typed with Shift such as "_".
// Those items show no shortcut; in a popup the shortcut is only a label.
const tauriAccelerator = (accelerator?: string): string | undefined => {
  const key = accelerator?.split('+').pop()
  return key && key !== 'Plus' && key !== '_' ? accelerator : undefined
}

const clicked = (item: ModelItem, siblings: ModelItem[]): void => {
  // Electron updates the item before calling its handler.
  if (item.type === 'checkbox') {
    item.checked = !item.checked
  } else if (item.type === 'radio') {
    for (const other of siblings) {
      if (other.type === 'radio') other.checked = other === item
    }
  }
  item.click?.(
    item as unknown as ElectronMenuItem,
    menuWindow as unknown as BaseWindow,
    {} as KeyboardEvent
  )
}

// Hidden items can leave separators at an edge or next to each other.
const tidySeparators = (items: ItemOptions[]): ItemOptions[] => {
  const isSeparator = (item: ItemOptions | undefined): boolean =>
    !!item && 'item' in item && item.item === 'Separator'
  const tidy = items.filter(
    (item, index) => !isSeparator(item) || (index > 0 && !isSeparator(items[index - 1]))
  )
  while (isSeparator(tidy[0])) tidy.shift()
  while (isSeparator(tidy[tidy.length - 1])) tidy.pop()
  return tidy
}

let nextItemId = 0

const toTauriItems = (items: ModelItem[]): ItemOptions[] => {
  const result: ItemOptions[] = []
  for (const item of items) {
    if (!item.visible) continue
    if (item.type === 'separator') {
      result.push({ item: 'Separator' })
      continue
    }
    const text = item.label ?? ''
    if (item.submenu) {
      result.push({ text, enabled: item.enabled, items: toTauriItems(item.submenu.items) })
      continue
    }
    const role = item.role ? ROLES[item.role] : undefined
    if (role && !item.click) {
      result.push({ item: role, text })
      continue
    }
    const options = {
      // Tauri routes clicks by id across all windows.
      id: `${appWindow.label}:app:${nextItemId++}`,
      text,
      enabled: item.enabled,
      accelerator: tauriAccelerator(item.accelerator),
      action: () => clicked(item, items)
    }
    result.push(
      item.type === 'checkbox' || item.type === 'radio'
        ? { ...options, checked: item.checked }
        : options
    )
  }
  return tidySeparators(result)
}

const showApplicationMenu = async(position?: MenuPopupPosition): Promise<void> => {
  // Electron shows debug entries in development (main/menu/templates/view.ts).
  ;(globalThis as Record<string, unknown>).MARKTEXT_DEBUG = boot.env.NODE_ENV === 'development'
  const recentFiles = await invoke<string[]>('recent_files')
  const model = toModel(buildEditorMenu(keybindings, preferenceStore, recentFiles))
  for (const update of updates.values()) update(model as unknown as ElectronMenu)
  await showPopup(toTauriItems(model.items), position)
}

// ---- channels -----------------------------------------------------------------

export const registerMenuHandlers = (): void => {
  onSend('mt::menu::popup-application', showApplicationMenu)

  // The renderer's menu updates; each starts with the window id.
  onSend('mt::update-format-menu', (_windowId: number, formats: Record<string, boolean>) =>
    record('format', (menu) => updateFormatMenu(menu, formats))
  )
  onSend('mt::editor-selection-changed', (_windowId: number, state: SelectionState) =>
    record('selection', (menu) => updateSelectionMenus(menu, state))
  )
  onSend('mt::set-editor-format-menus-enabled', (_windowId: number, enabled: boolean) =>
    record('formatEnabled', (menu) => setFormatMenusEnabled(menu, enabled))
  )
  onSend('mt::update-line-ending-menu', (_windowId: number, lineEnding: string) =>
    record('lineEnding', (menu) => setLineEnding(menu, lineEnding))
  )
  onSend('mt::update-sidebar-menu', (_windowId: number, value: unknown) =>
    record('sidebar', (menu) => updateSidebarMenu(menu, value))
  )
  onSend('mt::view-layout-changed', (_windowId: number, changes: Record<string, unknown>) => {
    viewLayout = { ...viewLayout, ...changes }
    const layout = viewLayout
    record('viewLayout', (menu) => viewLayoutChanged(menu, layout))
  })
  onSend('mt::add-recently-used-document', (pathname: string) =>
    invoke('add_recent_file', { path: pathname })
  )

  // Main process events that the reused menu actions emit.
  ipcMain.on('set-user-preference', setPreferences)
  ipcMain.on('window-toggle-always-on-top', async() => {
    await toggleAlwaysOnTop()
    const flag = alwaysOnTop()
    record('alwaysOnTop', (menu) => setAlwaysOnTop(menu, flag))
  })
  ipcMain.on('window-reload-by-id', () => location.reload())
}

// Native popup menus through Tauri's JS menu API. `popupMenu` stands in for
// packages/desktop/src/main/ipc/window.ts mt::menu::popup (the renderer's own
// context menus); src/appMenu.ts builds the ☰ menu with `showPopup`.

import { LogicalPosition } from '@tauri-apps/api/dpi'
import {
  Menu,
  type CheckMenuItemOptions,
  type MenuItemOptions,
  type PredefinedMenuItemOptions,
  type SubmenuOptions
} from '@tauri-apps/api/menu'
import type { MenuPopupPosition, MenuTemplate } from '@shared/types/menu'

export type ItemOptions =
  | MenuItemOptions
  | CheckMenuItemOptions
  | PredefinedMenuItemOptions
  | SubmenuOptions

// Tauri routes clicks by item id across the whole app, but each window
// numbers its renderer menu items from 1, so ids get a per window prefix.
const toItems = (
  template: MenuTemplate,
  prefix: string,
  onClick: (id: string) => void
): ItemOptions[] => {
  const items: ItemOptions[] = []
  for (const item of template) {
    if (item.visible === false) continue
    if (item.type === 'separator') {
      items.push({ item: 'Separator' })
      continue
    }
    const text = item.label ?? ''
    const enabled = item.enabled !== false
    if (item.submenu) {
      items.push({ text, enabled, items: toItems(item.submenu, prefix, onClick) })
      continue
    }
    const id = item.id ?? ''
    const options: MenuItemOptions = {
      id: `${prefix}${id}`,
      text,
      enabled,
      accelerator: item.accelerator || undefined,
      action: () => onClick(id)
    }
    // Tauri has no radio items; a check item shows the same state.
    if (item.type === 'checkbox' || item.type === 'radio') {
      items.push({ ...options, checked: !!item.checked })
    } else {
      items.push(options)
    }
  }
  return items
}

let shown: Menu | null = null

export const showPopup = async(items: ItemOptions[], position?: MenuPopupPosition): Promise<void> => {
  // The previous popup is gone once a new one opens; release it.
  await shown?.close()
  shown = await Menu.new({ items })
  await shown.popup(position ? new LogicalPosition(position.x, position.y) : undefined)
}

export const popupMenu = (
  template: MenuTemplate,
  position: MenuPopupPosition | undefined,
  prefix: string,
  onClick: (id: string) => void
): Promise<void> => showPopup(toItems(template, prefix, onClick), position)

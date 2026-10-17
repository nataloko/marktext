// Stand-in for packages/desktop/src/main/menu/actions/edit.ts, which also
// registers main process handlers and image path completion (Node fs) when it
// loads. The actions are the same; cut, copy and paste are menu roles that
// src/appMenu.ts hands to the webview.

import type { Menu } from 'electron'
import type { MenuWindow } from '../../../../appMenu'

type Win = MenuWindow | null | undefined

export const edit = (win: Win, type: string): void => {
  win?.webContents.send('mt::editor-edit-action', type)
}

export const editorUndo = (win: Win): void => edit(win, 'undo')
export const editorRedo = (win: Win): void => edit(win, 'redo')
export const editorCopyAsRich = (win: Win): void => edit(win, 'copyAsRich')
export const editorCopyAsHtml = (win: Win): void => edit(win, 'copyAsHtml')
export const editorPasteAsPlainText = (win: Win): void => edit(win, 'pasteAsPlainText')
export const editorSelectAll = (win: Win): void => edit(win, 'selectAll')
export const editorDuplicate = (win: Win): void => edit(win, 'duplicate')
export const editorCreateParagraph = (win: Win): void => edit(win, 'createParagraph')
export const editorDeleteParagraph = (win: Win): void => edit(win, 'deleteParagraph')
export const editorFind = (win: Win): void => edit(win, 'find')
export const editorFindNext = (win: Win): void => edit(win, 'findNext')
export const editorFindPrevious = (win: Win): void => edit(win, 'findPrev')
export const editorReplace = (win: Win): void => edit(win, 'replace')
export const findInFolder = (win: Win): void => edit(win, 'findInFolder')

// macOS only; the menu hides it elsewhere.
export const screenshot = (): void => {}

export const lineEnding = (win: Win, lineEnding: string): void => {
  win?.webContents.send('mt::set-line-ending', lineEnding)
}

export const updateSidebarMenu = (applicationMenu: Menu, value: unknown): void => {
  const sideBarMenuItem = applicationMenu.getMenuItemById('sideBarMenuItem')
  if (sideBarMenuItem) sideBarMenuItem.checked = !!value
}

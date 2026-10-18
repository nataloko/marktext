// Stand-in for packages/desktop/src/main/menu/actions/file.ts, which does its
// work with Node and Electron and registers many main process handlers when it
// loads. These are the actions the File menu calls; most ask the renderer, as
// the original does, and the rest use this window's backend modules.

import { invoke } from '@tauri-apps/api/core'
import type { MenuItem } from 'electron'
import type { MenuWindow } from '../../../../appMenu'
import { showOpenDialog } from '../../../../files'
import { openFileOrFolder as openPath, showOpenFolderDialog } from '../../../../folders'
import { exportWithPandoc as convertWithPandoc, importFile as importWithPandoc } from '../../../../pandoc'
import { setPreferences } from '../../../../preferences'
import { newWindow } from '../../../../window'

type Win = MenuWindow | null | undefined

const send = (win: Win, channel: string, ...args: unknown[]): void => {
  win?.webContents.send(channel, ...args)
}

export const newBlankTab = (win: Win): void => {
  send(win, 'mt::new-untitled-tab')
  send(win, 'mt::set-view-layout', { showTabBar: true })
}

export const newEditorWindow = (): void => {
  void newWindow()
}

export const openFile = (): void => {
  void showOpenDialog()
}

export const openFolder = (): void => {
  void showOpenFolderDialog()
}

export const openFileOrFolder = (_win: Win, pathname: string): void => {
  void openPath(pathname)
}

export const clearRecentlyUsed = (): void => {
  void invoke('clear_recent_files')
}

export const save = (win: Win): void => send(win, 'mt::editor-ask-file-save')
export const saveAs = (win: Win): void => send(win, 'mt::editor-ask-file-save-as')

export const autoSave = (menuItem: MenuItem): void => {
  void setPreferences({ autoSave: menuItem.checked })
}

export const moveTo = (win: Win): void => send(win, 'mt::editor-move-file')
export const rename = (win: Win): void => send(win, 'mt::editor-rename-file')
export const importFile = (): void => {
  void importWithPandoc()
}
export const exportWithPandoc = (_win: Win, id: string): void => {
  void convertWithPandoc(id)
}
export const exportFile = (win: Win, type: string): void => send(win, 'mt::show-export-dialog', type)
export const printDocument = (win: Win): void => send(win, 'mt::show-export-dialog', 'print')
export const closeTab = (win: Win): void => send(win, 'mt::editor-close-tab')

// Electron's win.close() reaches the renderer as a close request.
export const closeWindow = (win: Win): void => send(win, 'mt::ask-for-close')

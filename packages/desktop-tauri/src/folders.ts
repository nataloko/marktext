// The folder in this window's sidebar. Rust lists and watches it
// (src-tauri/src/folders.rs); the page hands its changes to the renderer.

import { invoke } from '@tauri-apps/api/core'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { appWindow } from './boot'
import { emitToRenderer } from './electron'
import { openTab } from './files'
import { preferences } from './preferences'

interface FolderBatch {
  generation: number
  // Set on the first batch of a folder.
  root: string | null
  changes: { type: string; change: unknown }[]
}

// Rust numbers folders in the order they are opened. Changes left over from a
// folder this window has since replaced are dropped.
let generation = 0

export const listenForFolder = (): Promise<unknown> =>
  appWindow.listen<FolderBatch>('mt::tauri::folder', ({ payload }) => {
    if (payload.root !== null && payload.generation > generation) {
      generation = payload.generation
      emitToRenderer('mt::open-directory', payload.root)
    }
    if (payload.generation !== generation) return
    for (const change of payload.changes) emitToRenderer('mt::update-object-tree', change)
  })

// packages/desktop/src/main/app/index.ts app-open-directory-by-id: the folder
// replaces this window's, unless the user prefers folders in new windows.
export const openFolder = async(pathname: string, sameWindow = false): Promise<void> => {
  try {
    if (!sameWindow && preferences.openFolderInNewWindow) {
      await invoke('new_editor_window', { files: [], folder: pathname })
    } else {
      await invoke('window_open_folder', { path: pathname })
    }
  } catch (err) {
    emitToRenderer('mt::show-notification', {
      title: 'Cannot open folder',
      type: 'error',
      message: String(err)
    })
  }
}

// main/menu/actions/file.ts openFolder. The sidebar's own button always
// opens the folder in this window.
export const showOpenFolderDialog = async(sameWindow = false): Promise<void> => {
  const picked = await openDialog({ directory: true })
  if (typeof picked === 'string') await openFolder(picked, sameWindow)
}

// file.ts openFileOrFolder, for the recent documents, which hold both.
export const openFileOrFolder = async(pathname: string): Promise<void> => {
  if (await invoke<boolean>('is_directory', { path: pathname })) await openFolder(pathname)
  else await openTab(pathname)
}

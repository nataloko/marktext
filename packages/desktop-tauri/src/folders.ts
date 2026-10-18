// The folder in this window's sidebar. Rust lists and watches it
// (src-tauri/src/folders.rs); the page hands its changes to the renderer.

import { invoke } from '@tauri-apps/api/core'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { appWindow } from './boot'
import { emitToRenderer } from './electron'
import { openTab, readDocument } from './files'
import { preferences } from './preferences'

interface TreeChange {
  type: string
  change: { pathname: string; data?: unknown }
}

interface FolderBatch {
  generation: number
  // Set on the first batch of a folder.
  root: string | null
  changes: TreeChange[]
}

// Rust numbers folders in the order they are opened. Changes left over from a
// folder this window has since replaced are dropped.
let generation = 0

// Files just created in the sidebar, with the time. The renderer opens such
// a file as the new tab from the content its add change brings (project.ts
// newFileNameCache), so this window opens it like any other document. An add
// that has not come soon will not come: the name may be excluded from the
// tree, or the window may show another folder by now.
const createdFiles = new Map<string, number>()
const CREATED_WAIT_MS = 10_000

export const expectNewFile = (pathname: string): void => {
  createdFiles.set(pathname, Date.now())
}

export const forgetNewFile = (pathname: string): void => {
  createdFiles.delete(pathname)
}

const justCreated = (pathname: string): boolean => {
  const created = createdFiles.get(pathname)
  createdFiles.delete(pathname)
  return created !== undefined && Date.now() - created < CREATED_WAIT_MS
}

const attachContent = async(change: TreeChange['change']): Promise<void> => {
  // False when a tab still has this path, from a file deleted before.
  const added = await invoke<boolean>('window_add_file', { path: change.pathname })
  try {
    change.data = await readDocument(change.pathname)
  } catch {
    if (added) void invoke('window_remove_file', { path: change.pathname })
  }
}

const deliver = async(batch: FolderBatch): Promise<void> => {
  if (batch.root !== null && batch.generation > generation) {
    generation = batch.generation
    emitToRenderer('mt::open-directory', batch.root)
  }
  if (batch.generation !== generation) return
  for (const item of batch.changes) {
    if (item.type === 'add' && justCreated(item.change.pathname)) {
      await attachContent(item.change)
    }
    emitToRenderer('mt::update-object-tree', item)
  }
}

// In order, since a new file's content is read on the way.
let delivering: Promise<void> = Promise.resolve()

export const listenForFolder = (): Promise<unknown> =>
  appWindow.listen<FolderBatch>('mt::tauri::folder', ({ payload }) => {
    delivering = delivering
      .then(() => deliver(payload))
      .catch((err) => console.error('[folders]', err))
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

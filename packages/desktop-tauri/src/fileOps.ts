// The renderer's file operations: window.fileUtils, the sidebar's Move to
// Trash and the menu's Rename and Move To. Rust does the work
// (src-tauri/src/fileops.rs); the questions Electron's main process asks
// before deleting or replacing a file are asked here.

import { invoke } from '@tauri-apps/api/core'
import { ask, save as saveDialog } from '@tauri-apps/plugin-dialog'
import { t } from '@/i18n'
import { path } from './boot'
import { emitToRenderer, onInvoke, onSend } from './electron'
import { expectNewFile, forgetNewFile } from './folders'

type Data = string | Uint8Array

// Invoke arguments travel as JSON, where a Uint8Array would become an object.
const serialize = (data: Data): string | number[] =>
  typeof data === 'string' ? data : Array.from(data)

// Electron makes Cancel the default button; this dialog cannot, so what the
// user agrees to lose goes to the trash (src-tauri/src/fileops.rs move_file).
const confirm = (message: string, okLabel: string): Promise<boolean> =>
  ask(message, { title: 'MarkText', kind: 'warning', okLabel, cancelLabel: t('dialog.cancel') })

// main/menu/actions/file.ts mt::rename and mt::response-file-move-to: the
// tab follows its file. Electron only logs a failure.
const moveDocument = async(id: string, pathname: string, target: string): Promise<void> => {
  try {
    await invoke('move_file', { from: pathname, to: target, replace: true, follow: true })
    emitToRenderer('mt::set-pathname', { id, pathname: target, filename: path.basename(target) })
  } catch (err) {
    emitToRenderer('mt::show-notification', {
      title: 'Cannot move the file',
      type: 'error',
      message: String(err)
    })
  }
}

interface MoveRequest {
  id: string
  pathname: string
  newPathname?: string
}

export const registerFileOperations = (): void => {
  onInvoke('mt::fs::is-file', (p: string) => invoke('fs_is_file', { path: p }))
  onInvoke('mt::fs::is-directory', (p: string) => invoke('fs_is_directory', { path: p }))
  onInvoke('mt::fs::is-executable', (p: string) => invoke('fs_is_executable', { path: p }))
  onInvoke('mt::fs::ensure-dir', (p: string) => invoke('fs_ensure_dir', { path: p }))
  onInvoke('mt::fs::readdir', (p: string) => invoke('fs_readdir', { path: p }))
  onInvoke('mt::fs::write-file', (p: string, data: Data) =>
    invoke('fs_write_file', { path: p, data: serialize(data) })
  )
  // The sidebar creates a new file as an empty one (project.ts CREATE_FILE_DIRECTORY).
  onInvoke('mt::fs::output-file', async(p: string, data: Data) => {
    if (data === '') expectNewFile(p)
    try {
      await invoke('fs_output_file', { path: p, data: serialize(data) })
    } catch (err) {
      forgetNewFile(p)
      throw err
    }
  })
  onInvoke('mt::fs::read-file', async(p: string, encoding?: string) => {
    const bytes = new Uint8Array(await invoke<ArrayBuffer>('fs_read_file', { path: p }))
    return encoding ? new TextDecoder(encoding === 'utf8' ? 'utf-8' : encoding).decode(bytes) : bytes
  })
  onInvoke('mt::fs::copy', (src: string, dest: string) => invoke('fs_copy', { from: src, to: dest }))
  // A rename in the sidebar keeps the folder and the renderer moves the tab
  // along (project.ts RENAME_IN_SIDEBAR); a paste after Cut leaves the tab on
  // the old path, as in Electron.
  onInvoke('mt::fs::move', (src: string, dest: string) =>
    invoke('move_file', {
      from: src,
      to: dest,
      replace: false,
      follow: path.dirname(src) === path.dirname(dest)
    })
  )

  // main/filesystem/trash.ts: true only when the item went to the trash.
  onInvoke('mt::fs-trash-item', async(fullPath: string) => {
    const message = t('dialog.moveToTrash', { name: path.basename(fullPath) })
    if (!(await confirm(message, t('contextMenu.sideBar.moveToTrash')))) return false
    await invoke('trash_item', { path: fullPath })
    return true
  })

  onSend('mt::rename', async({ id, pathname, newPathname }: MoveRequest) => {
    if (!newPathname || pathname === newPathname) return
    if (await invoke<boolean>('path_exists', { path: newPathname })) {
      const message = t('dialog.fileExists', { filename: path.basename(newPathname) })
      if (!(await confirm(message, t('dialog.replace')))) return
    }
    await moveDocument(id, pathname, newPathname)
  })

  // The save dialog asks before replacing a file.
  onSend('mt::response-file-move-to', async({ id, pathname }: MoveRequest) => {
    const target = await saveDialog({ title: 'Move to', defaultPath: pathname })
    if (target) await moveDocument(id, pathname, target)
  })
}

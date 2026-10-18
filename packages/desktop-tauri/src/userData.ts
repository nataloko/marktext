// The image settings. The store lives in Rust (src-tauri/src/user_data.rs) and
// broadcasts every change as `mt::tauri::user-data`; this window hands the
// changes to the renderer as Electron's dataCenter does, and answers the
// renderer's questions about image folders and files.

import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { emitToRenderer, onInvoke, onSend } from './electron'

type UserData = Record<string, unknown>

// common/filesystem/paths.ts IMAGE_EXTENSIONS.
const IMAGE_EXTENSIONS = ['jpeg', 'jpg', 'png', 'gif', 'svg', 'webp']

const setUserData = (changes: UserData): Promise<unknown> => invoke('set_user_data', { changes })

export const registerUserData = (): void => {
  onSend('mt::ask-for-user-data', async() =>
    emitToRenderer('mt::user-preference', await invoke<UserData>('get_user_data'))
  )
  onSend('mt::set-user-data', (changes: UserData) => {
    if (changes) return setUserData(changes)
  })
  // Without a path the user picks the folder; cancelling changes nothing.
  onSend('mt::ask-for-modify-image-folder-path', async(imagePath?: string) => {
    const folder =
      imagePath || (await openDialog({ directory: true, canCreateDirectories: true }))
    if (typeof folder === 'string' && folder) await setUserData({ imageFolderPath: folder })
  })
  // The renderer expects one path, or '' when the user cancels.
  onInvoke('mt::ask-for-image-path', async() => {
    const picked = await openDialog({
      filters: [{ name: 'Images', extensions: IMAGE_EXTENSIONS }]
    })
    return typeof picked === 'string' ? picked : ''
  })
  onInvoke('mt::paths::is-image', (p: string) => invoke('paths_is_image', { path: p }))
  onInvoke('mt::fs::copy-with-content-hash', (src: string, outputDir: string) =>
    invoke('fs_copy_with_content_hash', { src, outputDir })
  )
}

export const listenForUserData = (): Promise<unknown> =>
  listen<UserData>('mt::tauri::user-data', ({ payload }) => {
    emitToRenderer('mt::user-preference', payload)
  })

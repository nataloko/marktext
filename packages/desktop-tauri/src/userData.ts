// The image settings. The store lives in Rust (src-tauri/src/user_data.rs) and
// broadcasts every change as `mt::tauri::user-data`; this window hands the
// changes to the renderer as Electron's dataCenter does, and answers the
// renderer's questions about image folders and files.

import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { open as openDialog } from '@tauri-apps/plugin-dialog'
import { filter } from 'fuzzaldrin'
import { path } from './boot'
import { emitToRenderer, onInvoke, onSend } from './electron'

type UserData = Record<string, unknown>

// common/filesystem/paths.ts IMAGE_EXTENSIONS.
export const IMAGE_EXTENSIONS = ['jpeg', 'jpg', 'png', 'gif', 'svg', 'webp']

const setUserData = (changes: UserData): Promise<unknown> => invoke('set_user_data', { changes })

// renderer util/fileSystem.ts uploadImage.
interface UploadRequest {
  pathname: string
  image: string | { data: Uint8Array; name: string }
  preferences: { currentUploader: string; cliScript: string }
}

// Rust runs the uploader (src-tauri/src/uploader.rs). A Uint8Array would reach
// it as a JSON object, not a list of bytes.
const uploadImage = ({ pathname, image, preferences }: UploadRequest): Promise<string> =>
  invoke<string>('upload_image', {
    request: {
      pathname,
      image: typeof image === 'string' ? image : { data: Array.from(image.data), name: image.name },
      preferences
    }
  })

interface ImagePathEntry {
  file: string
  type: 'directory' | 'image'
}

interface ImagePathRequest {
  pathname: string
  src: unknown
  id: string
}

// main/menu/actions/edit.ts mt::ask-for-image-auto-path: the folders and
// images that could complete the image path being typed, `src`, which is
// relative to the open file. Rust lists the folder (fileops.rs image_path_entries).
const completeImagePath = async({ pathname, src, id }: ImagePathRequest): Promise<void> => {
  const reply = (entries: unknown[]): void =>
    emitToRenderer(`mt::response-of-image-path-${id}`, entries)
  if (!src || typeof src !== 'string') return reply([])
  const fullPath = path.isAbsolute(src) ? src : path.join(path.dirname(pathname), src)
  // A path ending in a separator lists the whole folder.
  const [dir, key] = fullPath.endsWith(path.sep)
    ? [fullPath.slice(0, -1), '']
    : [path.dirname(fullPath), path.basename(fullPath)]
  try {
    const entries = await invoke<ImagePathEntry[]>('image_path_entries', { dir })
    reply(filter(entries, key, { key: 'file' }))
  } catch (err) {
    console.error('[tauri] image path completion', err)
    reply([])
  }
}

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
  onSend('mt::ask-for-image-auto-path', completeImagePath)
  onInvoke('mt::uploader::upload', uploadImage)
}

export const listenForUserData = (): Promise<unknown> =>
  listen<UserData>('mt::tauri::user-data', ({ payload }) => {
    emitToRenderer('mt::user-preference', payload)
  })

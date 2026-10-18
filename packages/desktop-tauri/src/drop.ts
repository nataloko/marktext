// Files dropped onto an editor window from outside. Web pages cannot see the
// paths, so Rust reads them from the webview (src-tauri/src/drop.rs).
//
// Linux: WebKitGTK shows the page no files, so the renderer's import overlay
// and drop handler never run. Rust sends the dragged paths during the hover
// and this module takes the drop itself.
//
// Windows: the renderer's drop handler runs but gets empty paths. The dropped
// files go to Rust first, and `mt::window::drop` waits for their paths.
//
// Dropped images and folders are not handled.

import { invoke } from '@tauri-apps/api/core'
import { appWindow, boot, isWindows } from './boot'
import { onSend } from './electron'
import { openFileOrFolder } from './folders'
import { PANDOC_EXTENSIONS } from './stand-ins/main/config'
import { notAvailable } from './stand-ins/notAvailable'

interface WebView2Host {
  postMessageWithAdditionalObjects?: (message: unknown, objects: FileList) => void
}

declare global {
  interface Window {
    chrome?: { webview?: WebView2Host }
  }
}

const DROP_WAIT_MS = 5000

// main/menu/actions/file.ts mt::window::drop. The first file to import with
// pandoc ends the list.
const openDropped = async(paths: string[]): Promise<void> => {
  for (const pathname of paths) {
    if (await invoke<boolean>('is_markdown_file', { path: pathname })) {
      await openFileOrFolder(pathname)
      continue
    }
    if (PANDOC_EXTENSIONS.some((ext) => pathname.endsWith(ext))) {
      notAvailable('Import with pandoc')
      break
    }
  }
}

// ---- Linux -------------------------------------------------------------------

// The files of the drag over the window, empty for a drag without files.
let dragFiles: string[] = []

const takeDrops = (): void => {
  // A drag that starts in the page carries no outside files.
  window.addEventListener('dragstart', () => (dragFiles = []), true)
  window.addEventListener(
    'dragover',
    (e: DragEvent) => {
      if (!dragFiles.length || !e.dataTransfer) return
      // Without this WebKit allows no drop and loads the file in place of the editor.
      e.preventDefault()
      // The editor would take it for a web image and mark where it would go.
      e.stopImmediatePropagation()
      e.dataTransfer.dropEffect = 'copy'
    },
    true
  )
  window.addEventListener(
    'drop',
    (e: DragEvent) => {
      if (!dragFiles.length) return
      e.preventDefault()
      // The editor would insert the file's URI as a link.
      e.stopImmediatePropagation()
      // Clears the drop marker the editor drew before the paths arrived; it
      // removes it when the drag leaves the page (muya dragDropImage.ts).
      document.dispatchEvent(new Event('dragleave'))
      const paths = dragFiles
      dragFiles = []
      void openDropped(paths)
    },
    true
  )
}

// ---- Windows -----------------------------------------------------------------

let latestDrop = 0
let pendingPaths: Promise<string[]> | null = null
let resolvePending: ((paths: string[]) => void) | null = null

// Runs before the renderer's own drop handler, which then sends mt::window::drop.
const postDrops = (): void => {
  window.addEventListener(
    'drop',
    (e: DragEvent) => {
      const files = e.dataTransfer?.files
      const host = window.chrome?.webview
      if (!files?.length || !host?.postMessageWithAdditionalObjects) return
      const id = ++latestDrop
      resolvePending?.([])
      pendingPaths = new Promise((resolve) => {
        resolvePending = resolve
        setTimeout(() => resolve([]), DROP_WAIT_MS)
      })
      host.postMessageWithAdditionalObjects({ mtFileDrop: id }, files)
    },
    true
  )
}

const droppedPaths = async(): Promise<string[]> => {
  const pending = pendingPaths
  pendingPaths = null
  return pending ?? []
}

// ---- registration ------------------------------------------------------------

export const registerDrop = (): void => {
  onSend('mt::window::drop', async(fileList: string[]) => {
    const paths = fileList.every((pathname) => !!pathname) ? fileList : await droppedPaths()
    await openDropped(paths)
  })
}

export const listenForDrops = async(): Promise<void> => {
  if (boot.platform === 'linux') {
    await appWindow.listen<string[]>('mt::tauri::drag-files', ({ payload }) => {
      dragFiles = payload
    })
    takeDrops()
  } else if (isWindows) {
    await appWindow.listen<{ id: number; paths: string[] }>(
      'mt::tauri::dropped-files',
      ({ payload }) => {
        if (payload.id === latestDrop) resolvePending?.(payload.paths)
      }
    )
    postDrops()
  }
}

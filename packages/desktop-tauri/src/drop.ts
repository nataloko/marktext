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
// One image dropped onto the document is inserted where it was dropped, which
// Muya does in Electron (muya/src/editor/dragDropImage.ts) with the path it
// cannot get here. Dropped folders are not handled.

import { invoke } from '@tauri-apps/api/core'
import { appWindow, boot, isWindows } from './boot'
import { emitToRenderer, onSend } from './electron'
import { openFileOrFolder } from './folders'
import { importWithPandoc, noticePandocNotFound, pandocCommand } from './pandoc'
import { PANDOC_EXTENSIONS } from './stand-ins/main/config'
import { IMAGE_EXTENSIONS } from './userData'

interface WebView2Host {
  postMessageWithAdditionalObjects?: (message: string, objects: FileList) => void
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
      if (await pandocCommand()) await importWithPandoc(pathname)
      else noticePandocNotFound()
      break
    }
  }
}

// The document a drop landed on, null for a drop elsewhere or onto the source
// code editor.
const documentAt = (e: DragEvent): HTMLElement | null =>
  e.target instanceof Element ? e.target.closest<HTMLElement>('.mu-editor') : null

// Muya draws a drop marker for an image dragged over the document and removes
// it when the drag leaves the page (muya dragDropImage.ts).
const clearDropMarker = (): void => {
  document.dispatchEvent(new Event('dragleave'))
}

// The caret goes to the drop point and the renderer's screenshot hook inserts
// the image there, through the Image settings as a paste does
// (renderer editor.vue handleScreenShot). Muya in Electron inserts a dropped
// image as a paragraph of its own above or below the block instead. False
// when the file is not an image.
const insertImage = async(
  editor: HTMLElement,
  x: number,
  y: number,
  pathname: string
): Promise<boolean> => {
  if (!(await invoke<boolean>('paths_is_image', { path: pathname }))) return false
  const range = document.caretRangeFromPoint(x, y)
  editor.focus({ preventScroll: true })
  if (range && editor.contains(range.startContainer)) {
    range.collapse(true)
    const selection = window.getSelection()
    selection?.removeAllRanges()
    selection?.addRange(range)
  }
  emitToRenderer('mt::screenshot-captured', pathname)
  return true
}

const openOrInsert = async(
  paths: string[],
  editor: HTMLElement | null,
  x: number,
  y: number
): Promise<void> => {
  if (paths.length === 1 && editor && (await insertImage(editor, x, y, paths[0]))) return
  await openDropped(paths)
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
      // The editor may have drawn its drop marker before the paths arrived.
      clearDropMarker()
      const paths = dragFiles
      dragFiles = []
      void openOrInsert(paths, documentAt(e), e.clientX, e.clientY)
    },
    true
  )
}

// ---- Windows -----------------------------------------------------------------

let latestDrop = 0
let pendingPaths: Promise<string[]> | null = null
let resolvePending: ((paths: string[]) => void) | null = null

const hasImageExtension = (name: string): boolean =>
  IMAGE_EXTENSIONS.some((ext) => name.toLowerCase().endsWith(`.${ext}`))

// Runs before the renderer's own drop handler, which then sends
// mt::window::drop, and before Muya's, which would insert nothing for an image
// without its path.
const postDrops = (): void => {
  window.addEventListener(
    'drop',
    (e: DragEvent) => {
      const files = e.dataTransfer?.files
      const host = window.chrome?.webview
      if (!files?.length || !host?.postMessageWithAdditionalObjects) return
      // Decided before the path is known, so by the name.
      const editor = files.length === 1 && hasImageExtension(files[0].name) ? documentAt(e) : null
      if (editor) {
        e.preventDefault()
        e.stopImmediatePropagation()
        clearDropMarker()
      }
      const id = ++latestDrop
      resolvePending?.([])
      const paths = new Promise<string[]>((resolve) => {
        resolvePending = resolve
        setTimeout(() => resolve([]), DROP_WAIT_MS)
      })
      // A string, or Rust never sees it (src-tauri/src/drop.rs).
      host.postMessageWithAdditionalObjects(`mt-file-drop:${id}`, files)
      if (!editor) {
        pendingPaths = paths
        return
      }
      const { clientX, clientY } = e
      void paths.then(async(dropped) => {
        if (dropped.length === 1) await insertImage(editor, clientX, clientY, dropped[0])
      })
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

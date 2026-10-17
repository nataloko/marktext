// Opening and saving documents in this window. Rust keeps each window's open
// files (src-tauri/src/windows.rs) and the recent files (src-tauri/src/recent.rs).

import { invoke } from '@tauri-apps/api/core'
import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog'
import { boot, path } from './boot'
import { emitToRenderer } from './electron'
import { loadMarkdownFile, writeMarkdownFile, type MarkdownOptions } from './markdown'
import { preferences, preferredEol } from './preferences'

const MARKDOWN_EXTENSIONS = [
  'md',
  'markdown',
  'mdown',
  'mkdn',
  'mkd',
  'mdwn',
  'mdtxt',
  'mdtext',
  'mdx',
  'txt'
]

const addRecentFile = (pathname: string): Promise<unknown> =>
  invoke('add_recent_file', { path: pathname })

// packages/desktop/src/main/windows/editor.ts openTabsFromPaths and _doOpenTab.
export const openTab = async(pathname: string, selected = true): Promise<void> => {
  if (!(await invoke<boolean>('window_add_file', { path: pathname }))) {
    emitToRenderer('mt::switch-tab-by-file_path', pathname, {})
    return
  }
  try {
    const document = await loadMarkdownFile(pathname, {
      preferredEol: preferredEol(),
      trimTrailingNewline: Number(preferences.trimTrailingNewline ?? 2),
      autoNormalizeLineEndings: !!preferences.autoNormalizeLineEndings
    })
    void addRecentFile(pathname)
    emitToRenderer('mt::open-new-tab', document, {}, selected)
  } catch (err) {
    void invoke('window_remove_file', { path: pathname })
    emitToRenderer('mt::show-notification', {
      title: 'Cannot open tab',
      type: 'error',
      message: String(err)
    })
  }
}

export const openTabs = async(paths: string[]): Promise<void> => {
  for (const [index, pathname] of paths.entries()) {
    await openTab(pathname, index === 0)
  }
}

export const showOpenDialog = async(): Promise<void> => {
  const picked = await openDialog({
    multiple: true,
    filters: [{ name: 'Markdown', extensions: MARKDOWN_EXTENSIONS }]
  })
  await openTabs(picked === null ? [] : Array.isArray(picked) ? picked : [picked])
}

// Arguments of mt::response-file-save and mt::response-file-save-as.
export type SaveRequest = [
  id: string,
  filename: string,
  pathname: string,
  markdown: string,
  options: MarkdownOptions,
  defaultPath?: string
]

// packages/desktop/src/main/menu/actions/file.ts handleResponseForSave and Save As.
export const saveTab = async(
  forceDialog: boolean,
  ...[id, filename, pathname, markdown, options, defaultPath]: SaveRequest
): Promise<void> => {
  const existedOnDisk = !!pathname && (await invoke<boolean>('path_exists', { path: pathname }))
  let target = pathname

  if (forceDialog || !pathname) {
    const name = /\.[a-z0-9]+$/i.test(filename) ? filename : `${filename || 'Untitled'}.md`
    const directory = pathname ? path.dirname(pathname) : defaultPath || boot.env.HOME || ''
    const chosen = await saveDialog({
      defaultPath: directory ? path.join(directory, name) : name,
      filters: [{ name: 'Markdown', extensions: ['md', 'markdown'] }]
    })
    if (!chosen) return
    target = chosen
  }

  try {
    await writeMarkdownFile(target, markdown, options)
    if (target !== pathname || !existedOnDisk) {
      // file.ts emits window-change-file-path or window-add-file-path here,
      // and menu-add-recently-used.
      if (pathname && pathname !== target) {
        void invoke('window_change_file', { oldPath: pathname, path: target })
      } else {
        void invoke('window_add_file', { path: target })
      }
      void addRecentFile(target)
      emitToRenderer('mt::set-pathname', { id, pathname: target, filename: path.basename(target) })
    } else {
      emitToRenderer('mt::tab-saved', id)
    }
  } catch (err) {
    emitToRenderer('mt::tab-save-failure', id, String(err))
  }
}

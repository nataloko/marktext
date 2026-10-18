// Opening and saving documents in this window. Rust keeps each window's open
// files (src-tauri/src/windows.rs) and the recent files (src-tauri/src/recent.rs).

import { invoke } from '@tauri-apps/api/core'
import { ask, open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog'
import { t } from '@/i18n'
import { appWindow, boot, path } from './boot'
import { emitToRenderer } from './electron'
import {
  loadMarkdownFile,
  pendingMarkdownFile,
  writeMarkdownFile,
  type LoadPreferences,
  type MarkdownOptions
} from './markdown'
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

export const addRecentFile = (pathname: string): Promise<unknown> =>
  invoke('add_recent_file', { path: pathname })

const loadPreferences = (): LoadPreferences => ({
  preferredEol: preferredEol(),
  autoGuessEncoding: preferences.autoGuessEncoding !== false,
  trimTrailingNewline: Number(preferences.trimTrailingNewline ?? 2),
  autoNormalizeLineEndings: !!preferences.autoNormalizeLineEndings
})

export const readDocument = (pathname: string): ReturnType<typeof loadMarkdownFile> =>
  loadMarkdownFile(pathname, loadPreferences())

// The document for a file that does not exist yet; its folder has to.
const pendingDocument = async(pathname: string): Promise<ReturnType<typeof pendingMarkdownFile>> => {
  const folder = path.dirname(pathname)
  if (!(await invoke<boolean>('path_exists', { path: folder }))) {
    throw new Error(`${folder}: no such folder`)
  }
  return pendingMarkdownFile(pathname, loadPreferences(), String(preferences.defaultEncoding ?? 'utf8'))
}

// packages/desktop/src/main/windows/editor.ts openTabsFromPaths and _doOpenTab.
// With `allowMissing`, for files named on the command line, a file that does
// not exist yet opens as an empty document under its name; it is left out of
// the recent files and not watched until it is saved.
export const openTab = async(
  requested: string,
  selected = true,
  allowMissing = false
): Promise<void> => {
  const pathname = await invoke<string>('resolve_path', { path: requested })
  const missing = allowMissing && !(await invoke<boolean>('path_exists', { path: pathname }))
  if (!(await invoke<boolean>('window_add_file', { path: pathname, watch: !missing }))) {
    emitToRenderer('mt::switch-tab-by-file_path', pathname, {})
    return
  }
  try {
    const document = missing
      ? await pendingDocument(pathname)
      : await loadMarkdownFile(pathname, loadPreferences())
    if (!missing) void addRecentFile(pathname)
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

export const openTabs = async(paths: string[], allowMissing = false): Promise<void> => {
  for (const [index, pathname] of paths.entries()) {
    await openTab(pathname, index === 0, allowMissing)
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
  options: MarkdownOptions & { isPendingCreation?: boolean },
  defaultPath?: string
]

// packages/desktop/src/main/menu/actions/file.ts handleResponseForSave and Save As.
export const saveTab = async(
  forceDialog: boolean,
  ...[id, filename, pathname, markdown, options, defaultPath]: SaveRequest
): Promise<void> => {
  // A document opened from the command line before its file existed.
  const pending = !!pathname && !!options.isPendingCreation
  // file.ts handleResponseForSave: if that file has appeared since, it is
  // replaced only when the user agrees. Save As leaves it to the save dialog.
  if (pending && !forceDialog && (await invoke<boolean>('path_exists', { path: pathname }))) {
    const replace = await ask(t('dialog.fileExists', { filename: pathname }), {
      title: 'MarkText',
      kind: 'warning',
      okLabel: t('dialog.replace'),
      cancelLabel: t('dialog.cancel')
    })
    if (!replace) return
  }
  const existedOnDisk =
    !pending && !!pathname && (await invoke<boolean>('path_exists', { path: pathname }))
  let target = pathname

  if (forceDialog || !pathname) {
    const name = /\.[a-z0-9]+$/i.test(filename) ? filename : `${filename || 'Untitled'}.md`
    const directory = pathname ? path.dirname(pathname) : defaultPath || boot.env.HOME || ''
    const chosen = await saveDialog({
      defaultPath: directory ? path.join(directory, name) : name,
      filters: [{ name: 'Markdown', extensions: ['md', 'markdown'] }]
    })
    if (!chosen) return
    // file.ts handleResponseForSave: a name without an extension gets ".md".
    target = /\.[^\\/.]+$/.test(chosen) ? chosen : `${chosen}.md`
  }

  try {
    await writeMarkdownFile(target, markdown, options)
    if (target !== pathname || !existedOnDisk) {
      // file.ts emits window-change-file-path or window-add-file-path here,
      // and menu-add-recently-used.
      if (pathname && pathname !== target) {
        void invoke('window_change_file', { oldPath: pathname, path: target })
      } else if (pending) {
        void invoke('window_watch_file', { path: target })
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

interface FileEvent {
  pathname: string
  kind: 'add' | 'change' | 'unlink'
  mtimeMs: number
}

// Outside changes to this window's files (src-tauri/src/watch.rs), sent to the
// renderer as packages/desktop/src/main/filesystem/watcher.ts sends them.
export const listenForFileChanges = (): Promise<unknown> =>
  appWindow.listen<FileEvent>('mt::tauri::file-changed', async({ payload }) => {
    const { pathname, kind, mtimeMs } = payload
    if (kind === 'unlink') {
      emitToRenderer('mt::update-file', { type: 'unlink', change: { pathname } })
      return
    }
    try {
      const data = await loadMarkdownFile(pathname, loadPreferences())
      const change =
        kind === 'add'
          ? {
            pathname,
            name: path.basename(pathname),
            isFile: true,
            isDirectory: false,
            mtimeMs,
            isMarkdown: true,
            data
          }
          : { pathname, data, mtimeMs }
      emitToRenderer('mt::update-file', { type: kind, change })
    } catch (err) {
      emitToRenderer('mt::show-notification', {
        title: 'Watcher I/O error',
        type: 'error',
        message: String(err)
      })
    }
  })

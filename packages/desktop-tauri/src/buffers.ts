// This window's session store for the "Restore all" start up action. Rust
// keeps one file per editor window (src-tauri/src/buffers.rs); the page writes
// the state the renderer builds and, in a restored window, reads it back as
// packages/desktop/src/main/windows/editor.ts _restoreAllState does.

import { invoke } from '@tauri-apps/api/core'
import { message } from '@tauri-apps/plugin-dialog'
import { emitToRenderer, whenListening } from './electron'
import { addRecentFile, readDocument } from './files'
import { showFolder } from './folders'

interface RestoredTab {
  pathname?: unknown
  filename?: string
  markdown?: string
  isSaved?: boolean
  isPendingCreation?: boolean
}

interface RestoredState {
  tabs: unknown[]
  restoreWarnings?: unknown[]
  project?: { rootDirectory?: string } | null
}

type Loaded =
  | { kind: 'state'; state: RestoredState }
  | { kind: 'missing' }
  | { kind: 'damaged'; kept: string; reason: string }
  | { kind: 'unreadable'; reason: string }

// Until the window's session is known, the renderer's state is not the
// session, and writing it could replace the store with an empty window.
let restoring = true
// Set once the window is closing, so that a late debounced write cannot bring
// back a store that closing deleted.
let closing = false

// Writes reach Rust one after another, in the order the renderer made them.
let writing: Promise<unknown> = Promise.resolve()
let lastWriteError: unknown = null
let writeFailing = false

const notifyError = (title: string, text: string): void => {
  emitToRenderer('mt::show-notification', { title, type: 'error', message: text })
}

// A lone UTF-16 surrogate would become JSON text that Rust refuses.
const wellFormed = (text: string): string => {
  const native = (text as string & { toWellFormed?: () => string }).toWellFormed
  if (native) return native.call(text)
  return text.replace(/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/g, '\uFFFD')
}

// A window that is not restored writes from the start.
export const allowWrites = (): void => {
  restoring = false
}

// `update-buffer-state`. The state is turned into text at once, so a later
// change to the renderer's objects cannot reach a write still in the queue.
export const updateBufferState = (state: unknown): Promise<boolean> => {
  if (restoring || closing) return Promise.resolve(true)
  const text = JSON.stringify(state, (_key, value: unknown) =>
    typeof value === 'string' ? wellFormed(value) : value
  )
  const write = writing.then(() => invoke<boolean>('update_buffer_state', { state: text }))
  writing = write.then(
    () => {
      lastWriteError = null
      writeFailing = false
    },
    (err) => {
      lastWriteError = err
      console.error('[buffers] cannot save the session', err)
      // Once per run of failures.
      if (writeFailing) return
      writeFailing = true
      notifyError(
        'Could not save the session',
        `${String(err)}. Unsaved changes in this window may not come back after a restart.`
      )
    }
  )
  return write
}

// Waits for the queued writes and says why closing now could lose text, if it could.
export const closeRisk = async(): Promise<string | null> => {
  await writing
  if (restoring) return 'This window has not saved its session.'
  return lastWriteError === null ? null : `The session could not be saved (${String(lastWriteError)}).`
}

// Just before the window closes: Rust deletes the store when the user
// discards the unsaved changes, or when nothing is lost without it.
export const closeBuffer = async(discard: boolean): Promise<void> => {
  closing = true
  await writing
  try {
    await invoke('close_buffer', { discard })
  } catch (err) {
    console.error('[buffers] cannot close the session store', err)
    // The window is about to go, so a notification would not be seen.
    if (discard) {
      await message(
        `The saved session could not be deleted (${String(err)}). The discarded changes will come back at the next start.`,
        { title: 'MarkText', kind: 'warning' }
      )
    }
  }
}

// The user kept the window open, which also ends a quit.
export const cancelClose = (): Promise<unknown> => invoke('close_cancelled')

// Folder in the sidebar first, so that the renderer's project restore finds
// it open: the root Rust resolved replaces the stored one, which would
// otherwise reopen the folder and drop the tree listed so far. A folder that
// cannot open is dropped, so the renderer does not keep saving it.
const openProject = async(state: RestoredState): Promise<void> => {
  const rootDirectory = state.project?.rootDirectory
  if (!rootDirectory) return
  try {
    const shown = await showFolder(rootDirectory)
    if (shown) state.project = { ...state.project, rootDirectory: shown }
  } catch (err) {
    state.project = null
    notifyError('Cannot open folder', String(err))
  }
}

// Saved tabs take the text on disk; a tab with unsaved changes keeps its own.
const reloadTab = async(tab: RestoredTab & { pathname: string }): Promise<void> => {
  try {
    const document = await readDocument(tab.pathname)
    // A dirty tab that was never saved still asks before replacing a file
    // that now exists.
    if (tab.isSaved) {
      tab.isPendingCreation = false
      tab.markdown = document.markdown
    }
    if (await invoke<boolean>('window_add_file', { path: tab.pathname })) {
      void addRecentFile(tab.pathname)
    }
  } catch (err) {
    if (tab.isPendingCreation && !(await invoke<boolean>('path_exists', { path: tab.pathname }))) {
      await invoke('window_add_file', { path: tab.pathname })
      return
    }
    tab.isSaved = false
    notifyError(`Could not find file ${tab.filename} on disk, please save your work.`, String(err))
  }
}

const isTab = (tab: unknown): tab is RestoredTab => typeof tab === 'object' && tab !== null
const hasPath = (tab: RestoredTab): tab is RestoredTab & { pathname: string } =>
  typeof tab.pathname === 'string' && tab.pathname !== ''

// The renderer gets the session, or a blank tab when there is none to restore.
// Never throws: a failure leaves the store as it was and the window going on
// under a new one.
export const restoreSession = async(bootstrap: (addBlankTab: boolean) => void): Promise<void> => {
  let bootstrapped = false
  try {
    const loaded = await invoke<Loaded>('load_buffer_state')
    if (loaded.kind !== 'state') {
      // Rust moved a damaged store aside and gave an unreadable one's window
      // a new id, so writes can no longer harm a stored session.
      restoring = false
      bootstrapped = true
      bootstrap(true)
      if (loaded.kind === 'damaged') {
        notifyError(
          'Could not restore the session',
          `The saved session could not be read (${loaded.reason}). The file was kept at ${loaded.kept}.`
        )
      } else if (loaded.kind === 'unreadable') {
        notifyError(
          'Could not restore the session',
          `The saved session could not be read (${loaded.reason}). It was left as it was.`
        )
      }
      return
    }

    const { state } = loaded
    state.tabs = state.tabs.filter(isTab)
    if (!Array.isArray(state.restoreWarnings)) state.restoreWarnings = []
    bootstrapped = true
    bootstrap(false)
    await openProject(state)
    // The session reaches the renderer whatever happens to one tab.
    const withPath = (state.tabs as RestoredTab[]).filter(hasPath)
    await Promise.all(
      withPath.map((tab) =>
        reloadTab(tab).catch((err) => {
          tab.isSaved = false
          console.error('[buffers] cannot restore', tab.pathname, err)
        })
      )
    )

    await whenListening('mt::load-state')
    emitToRenderer('mt::load-state', state)
    // Delivered on a later task (./electron); writes resume after it.
    setTimeout(allowWrites, 0)
  } catch (err) {
    console.error('[buffers] cannot restore the session', err)
    if (bootstrapped) emitToRenderer('mt::new-untitled-tab')
    else bootstrap(true)
    try {
      await invoke('abandon_buffer')
      allowWrites()
      notifyError(
        'Could not restore the session',
        `${String(err)}. The saved session was left as it was for the next start.`
      )
    } catch (abandonErr) {
      // Writes stay held back, so the store is not overwritten, and closing asks.
      notifyError(
        'Could not restore the session',
        `${String(err)}. This window cannot save its session (${String(abandonErr)}).`
      )
    }
  }
}

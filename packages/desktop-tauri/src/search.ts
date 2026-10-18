// Find in folder and quick open (packages/desktop/src/main/ipc/ripgrep.ts).
// Rust searches (src-tauri/src/search.rs) and sends the mt::rg::* events to
// the renderer as renderer messages.

import { invoke } from '@tauri-apps/api/core'
import { boot } from './boot'
import { onInvoke, onSend } from './electron'
import { openTab } from './files'
import { preferences } from './preferences'

export const registerSearch = (): void => {
  onInvoke('mt::rg::start', (request: unknown) => invoke<boolean>('search_start', { request }))
  onSend('mt::rg::cancel', (searchId: string) => invoke('search_cancel', { searchId }))

  // main/app/index.ts mt::open-file-by-window-id, for a quick open result.
  // The new window's page resolves the path as it opens the tab.
  onSend('mt::open-file-by-window-id', async(windowId: number, filePath: string) => {
    if (windowId !== boot.windowId) return
    if (preferences.openFilesInNewWindow) {
      await invoke('new_editor_window', { files: [filePath] })
    } else {
      await openTab(filePath)
    }
  })
}

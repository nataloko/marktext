// On Linux the paste keys go through Rust (main.rs editing_command), which
// pastes only when the clipboard offers something: WebKitGTK crashes when it
// pastes from a clipboard that offers nothing (src-tauri/src/clipboard.rs
// when_pasteable). The page still gets its usual paste event.

import { invoke } from '@tauri-apps/api/core'
import { boot } from './boot'

const isPasteKey = (event: KeyboardEvent): boolean => {
  if (event.altKey || event.metaKey) return false
  if (event.ctrlKey && !event.shiftKey) {
    return event.key.toLowerCase() === 'v' || event.code === 'KeyV'
  }
  return event.shiftKey && !event.ctrlKey && event.key === 'Insert'
}

const MIDDLE_BUTTON = 1

export const installPasteGuard = (): void => {
  if (boot.platform !== 'linux') return
  window.addEventListener(
    'keydown',
    (event) => {
      if (!isPasteKey(event)) return
      event.preventDefault()
      void invoke('editing_command', { command: 'paste' })
    },
    true
  )
  // A middle click would paste X11's primary selection the same way, and
  // crash when nothing is selected anywhere, so it pastes nothing. Listeners
  // such as the tabs' middle click to close still run.
  for (const type of ['mousedown', 'mouseup', 'auxclick']) {
    window.addEventListener(
      type,
      (event) => {
        if ((event as MouseEvent).button === MIDDLE_BUTTON) event.preventDefault()
      },
      true
    )
  }
}

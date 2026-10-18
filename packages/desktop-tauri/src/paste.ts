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
}

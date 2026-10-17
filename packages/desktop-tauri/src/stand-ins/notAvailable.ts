// Tells the user that a menu command has no Tauri counterpart yet.

import { emitToRenderer } from '../electron'

export const notAvailable = (command: string): void => {
  emitToRenderer('mt::show-notification', {
    title: command,
    type: 'warning',
    message: 'Not available in the Tauri trial yet.'
  })
}

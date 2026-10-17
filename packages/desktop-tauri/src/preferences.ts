// This window's copy of the preferences. The store lives in Rust
// (src-tauri/src/preferences.rs) and broadcasts every change as
// `mt::user-preference`.

import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { isWindows } from './boot'
import { emitToRenderer } from './electron'

export type Preferences = Record<string, unknown>

export let preferences: Preferences = {}

export const loadPreferences = async(): Promise<void> => {
  // Only used on first run, to pick the theme (main/preferences/index.ts init).
  const prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches
  preferences = await invoke<Preferences>('get_preferences', { prefersDark })
}

export const setPreferences = (changes: Preferences): Promise<unknown> =>
  invoke('set_preferences', { changes })

// What the renderer sees, for the full set or a change. Session restore is not
// implemented in the trial, so "restoreAll" would let the window close without
// asking about unsaved tabs (store/editor.ts LISTEN_FOR_CLOSE).
export const forRenderer = (prefs: Preferences): Preferences => {
  const shown = { ...prefs }
  if (shown.startUpAction === 'restoreAll') shown.startUpAction = 'blank'
  delete shown.titleBarStyle
  return shown
}

export const preferredEol = (): 'lf' | 'crlf' => {
  const endOfLine = preferences.endOfLine
  if (endOfLine === 'lf') return 'lf'
  return endOfLine === 'crlf' || isWindows ? 'crlf' : 'lf'
}

export const listenForPreferences = (): Promise<unknown> =>
  listen<Preferences>('mt::user-preference', ({ payload }) => {
    preferences = { ...preferences, ...payload }
    emitToRenderer('mt::user-preference', forRenderer(payload))
  })

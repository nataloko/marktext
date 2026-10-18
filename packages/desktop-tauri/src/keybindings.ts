// The shortcut table every window shares: the platform defaults overlaid with
// the user's keybindings.json (stored by src-tauri/src/keybindings.rs, which
// broadcasts each save). The merge is
// packages/desktop/src/main/keyboard/shortcutHandler.ts.

import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import linuxKeybindings from '../../desktop/src/main/keyboard/keybindingsLinux'
import windowsKeybindings from '../../desktop/src/main/keyboard/keybindingsWindows'
import { boot } from './boot'
import { emitToRenderer } from './electron'

export const defaultKeybindings = (platform: string): Map<string, string> =>
  platform === 'win32' ? windowsKeybindings : linuxKeybindings

export interface MergedKeybindings {
  keys: Map<string, string>
  // The user's entries, plus the defaults they took over (as ''), for the settings page.
  userKeybindings: Map<string, string>
}

interface AcceleratorChecks {
  isValidAccelerator: (accelerator: string) => boolean
  isEqualAccelerator: (a: string, b: string) => boolean
}

// Keep in step with Keybindings._loadLocalKeybindings. It is pure so it can run
// in the page; the accelerator checks are passed in because their modules read
// `process` as they load, which exists only once the preload has run.
export const mergeKeybindings = (
  defaults: Map<string, string>,
  raw: Record<string, unknown>,
  { isValidAccelerator, isEqualAccelerator }: AcceleratorChecks
): MergedKeybindings => {
  const unchanged = { keys: new Map(defaults), userKeybindings: new Map<string, string>() }

  const userAccelerators = new Map<string, string>()
  for (const [id, value] of Object.entries(raw)) {
    if (!defaults.has(id) || typeof value !== 'string') continue
    if (value.length === 0) {
      userAccelerators.set(id, '')
    } else if (isValidAccelerator(value)) {
      userAccelerators.set(id, value)
    } else {
      console.error(`[WARNING] "${value}" is not a valid accelerator.`)
    }
  }

  for (const [idA, valueA] of userAccelerators) {
    for (const [idB, valueB] of userAccelerators) {
      if (valueA !== '' && idA !== idB && isEqualAccelerator(valueA, valueB)) {
        console.error(
          `Invalid keybindings.json configuration: Duplicate value for "${idA}" and "${idB}"!`
        )
        return unchanged
      }
    }
  }
  if (userAccelerators.size === 0) return unchanged

  const keys = new Map(defaults)
  for (const [userId, userValue] of userAccelerators) {
    // An empty value means "unbound" and must not match other empty defaults.
    if (userValue) {
      for (const [id, value] of keys) {
        // A default that uses the accelerator is unbound, and shown as such in settings.
        if (isEqualAccelerator(value, userValue)) {
          keys.set(id, '')
          if (userAccelerators.get(id) == null) userAccelerators.set(id, '')
          break
        }
      }
    }
    keys.set(userId, userValue)
  }
  return { keys, userKeybindings: userAccelerators }
}

// Shared by shortcuts, the ☰ menu and the renderer's keybinding replies. It is
// updated in place, so holders never see a stale copy.
export const keybindings: Map<string, string> = new Map(defaultKeybindings(boot.platform))
export let userKeybindings: Map<string, string> = new Map()

const listeners = new Set<() => void>()
export const onKeybindingsChanged = (listener: () => void): void => {
  listeners.add(listener)
}

export const sendKeybindings = (): void =>
  emitToRenderer('mt::keybindings-response', Object.fromEntries(keybindings))

const apply = (raw: Record<string, unknown>, checks: AcceleratorChecks): void => {
  const merged = mergeKeybindings(defaultKeybindings(boot.platform), raw, checks)
  keybindings.clear()
  for (const [id, accelerator] of merged.keys) keybindings.set(id, accelerator)
  userKeybindings = merged.userKeybindings
  listeners.forEach((listener) => listener())
}

// Runs after the preload, which defines `process` for the checks' modules.
export const loadKeybindings = async(): Promise<void> => {
  const { isValidElectronAccelerator } = await import('@hfelix/electron-localshortcut')
  const { isEqualAccelerator } = await import('common/keybinding')
  const checks = { isValidAccelerator: isValidElectronAccelerator, isEqualAccelerator }
  await listen<Record<string, unknown>>('mt::tauri::user-keybindings', ({ payload }) => {
    apply(payload, checks)
    sendKeybindings()
  })
  apply(await invoke<Record<string, unknown>>('read_user_keybindings'), checks)
}

// Resolves to whether the file was written.
export const saveUserKeybindings = async(user: Map<string, string>): Promise<boolean> => {
  try {
    await invoke('write_user_keybindings', { values: Object.fromEntries(user) })
    return true
  } catch (err) {
    console.error('[keybindings] could not save:', err)
    return false
  }
}

export const ensureKeybindingsFile = (): Promise<string> => invoke('ensure_keybindings_file')

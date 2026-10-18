// In Electron, main registers every menu accelerator per window and turns a
// key press into a command (packages/desktop/src/main/keyboard). Tauri has no
// such layer, so this listens in the page and maps the same default tables.

import { onKeybindingsChanged } from './keybindings'

interface Accelerator {
  ctrl: boolean
  alt: boolean
  shift: boolean
  // Keys whose character already implies Shift ("Plus", "_") skip the Shift check.
  ignoreShift: boolean
  key: string
}

const parseAccelerator = (accelerator: string): Accelerator | null => {
  if (!accelerator) return null
  const parts = accelerator.split('+')
  const key = (parts.pop() ?? '').toLowerCase()
  const mods = new Set(parts.map((part) => part.toLowerCase()))
  return {
    ctrl: mods.has('ctrl') || mods.has('cmdorctrl') || mods.has('commandorcontrol'),
    alt: mods.has('alt'),
    shift: mods.has('shift'),
    ignoreShift: key === 'plus' || key === '_',
    key
  }
}

const matchesKey = (key: string, event: KeyboardEvent): boolean => {
  if (/^[a-z]$/.test(key)) {
    return event.key.toLowerCase() === key || event.code === `Key${key.toUpperCase()}`
  }
  if (/^[0-9]$/.test(key)) return event.code === `Digit${key}`
  if (key === 'plus') return event.key === '+'
  return event.key.toLowerCase() === key
}

const matches = (accelerator: Accelerator, event: KeyboardEvent): boolean =>
  event.ctrlKey === accelerator.ctrl &&
  event.altKey === accelerator.alt &&
  !event.metaKey &&
  (accelerator.ignoreShift || event.shiftKey === accelerator.shift) &&
  matchesKey(accelerator.key, event)

const isTextField = (target: EventTarget | null): boolean =>
  target instanceof HTMLInputElement || target instanceof HTMLTextAreaElement

export const installShortcuts = (
  keybindings: Map<string, string>,
  runCommand: (id: string) => boolean
): void => {
  let table: { id: string; accelerator: Accelerator }[] = []
  const rebuild = (): void => {
    table = [...keybindings]
      .map(([id, accelerator]) => ({ id, accelerator: parseAccelerator(accelerator) }))
      .filter((entry): entry is { id: string; accelerator: Accelerator } => !!entry.accelerator)
  }
  rebuild()
  onKeybindingsChanged(rebuild)

  window.addEventListener(
    'keydown',
    (event) => {
      const entry = table.find(({ accelerator }) => matches(accelerator, event))
      if (!entry) return
      // Plain text fields (search box, dialogs, CodeMirror's hidden textarea)
      // keep their own undo, redo and select all.
      if (entry.id.startsWith('edit.') && isTextField(event.target)) return
      if (runCommand(entry.id)) {
        event.preventDefault()
        event.stopPropagation()
      }
    },
    true
  )
}

// The editor's right click menu. Electron builds it in
// packages/desktop/src/main/contextMenu/editor from the parameters of the
// webContents `context-menu` event; this page works those parameters out from
// the DOM event and runs the same code. Everywhere else the webview's own
// menu is suppressed, as Electron has none.

import type { BrowserWindow } from 'electron'
import { showEditorContextMenu } from '../../desktop/src/main/contextMenu/editor'
import { popupTemplate } from './appMenu'
import { boot, isSettings } from './boot'
import { setMenuPopupHandler } from './electron'

// Debug builds let Shift + right click through, for Inspect Element.
const allowsWebviewMenu = (event: MouseEvent): boolean =>
  event.shiftKey && boot.env.NODE_ENV === 'development'

const editorParams = (event: MouseEvent) => {
  const target = event.target instanceof Element ? event.target : null
  const inEditor = !!target?.closest('.mu-editor')
  const inputField = target?.closest('input, textarea')
  const selectionText = window.getSelection()?.toString() ?? ''
  const hasSelection = selectionText.trim().length > 0
  return {
    isEditable: inEditor,
    hasImageContents: !!target?.closest('img'),
    selectionText,
    inputFieldType: inputField ? 'plainText' : undefined,
    editFlags: {
      canCut: inEditor && hasSelection,
      canCopy: inEditor && hasSelection,
      canPaste: true,
      canEditRichly: inEditor
    },
    x: event.clientX,
    y: event.clientY
  }
}

export const installContextMenu = (): void => {
  if (!isSettings) setMenuPopupHandler(popupTemplate)

  window.addEventListener('contextmenu', (event) => {
    if (allowsWebviewMenu(event)) return
    // Listeners that ran first, such as CodeMirror and the renderer's own
    // menus, have already handled the event.
    const handled = event.defaultPrevented
    event.preventDefault()
    if (handled || isSettings) return
    try {
      showEditorContextMenu(
        {} as BrowserWindow,
        event,
        editorParams(event),
        false
      )
    } catch (err) {
      console.error('[tauri] editor context menu failed', err)
    }
  })
}

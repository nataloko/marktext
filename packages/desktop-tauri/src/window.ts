// This window's controls and the window events the renderer listens for.

import { invoke } from '@tauri-apps/api/core'
import { appWindow } from './boot'
import { emitToRenderer } from './electron'

let isFullscreen = false
let isAlwaysOnTop = false

export const fullscreen = (): boolean => isFullscreen
export const alwaysOnTop = (): boolean => isAlwaysOnTop

export const setFullscreen = async(flag: boolean): Promise<void> => {
  await appWindow.setFullscreen(flag)
  isFullscreen = flag
  emitToRenderer(flag ? 'mt::window-enter-full-screen' : 'mt::window-leave-full-screen')
}

// packages/desktop/src/main/app/windowManager.ts window-toggle-always-on-top.
export const toggleAlwaysOnTop = async(): Promise<void> => {
  await appWindow.setAlwaysOnTop(!isAlwaysOnTop)
  isAlwaysOnTop = !isAlwaysOnTop
}

export const newWindow = (): Promise<unknown> => invoke('new_editor_window', { files: [] })

export const closeWindow = async(): Promise<void> => {
  await appWindow.destroy()
}

// Closes every window, each asking about its own unsaved documents.
export const quitApp = (): Promise<unknown> => invoke('quit_app')

export const installWindowEvents = async(): Promise<void> => {
  let wasMaximized = await appWindow.isMaximized()
  await appWindow.onResized(async() => {
    const maximized = await appWindow.isMaximized()
    if (maximized === wasMaximized) return
    wasMaximized = maximized
    emitToRenderer(maximized ? 'mt::window-maximize' : 'mt::window-unmaximize')
  })
  await appWindow.onFocusChanged(({ payload }) => {
    emitToRenderer('mt::window-active-status', { status: payload })
  })
  // The window manager's close (Alt+F4) asks the renderer about unsaved tabs first.
  await appWindow.onCloseRequested((event) => {
    event.preventDefault()
    emitToRenderer('mt::ask-for-close')
  })

  // Electron copies document.title into the window title; Tauri does not.
  const title = document.querySelector('title')
  if (title) {
    new MutationObserver(() => appWindow.setTitle(document.title)).observe(title, {
      childList: true,
      characterData: true,
      subtree: true
    })
  }

  // Replaces `-webkit-app-region: drag` on the custom title bar
  // (components/titleBar/index.vue), which only Chromium understands.
  const noDrag = '.title-no-drag, .frameless-titlebar-button, .word-count, button, input, a'
  window.addEventListener('mousedown', (event) => {
    const target = event.target as Element | null
    if (event.button !== 0 || !target?.closest('.title-bar') || target.closest(noDrag)) return
    if (event.detail === 2) appWindow.toggleMaximize()
    else appWindow.startDragging()
  })
}

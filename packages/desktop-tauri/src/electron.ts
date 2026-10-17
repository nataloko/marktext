// Stand-in for the slice of Electron's renderer API that
// packages/desktop/src/preload/index.ts imports. Vite aliases `electron` to
// this file, so the preload runs unchanged inside the Tauri page and keeps
// building the same `window.*` globals the renderer expects. Whatever the
// preload sends "to main" lands in the handlers registered by ./backend.
// The end of the file stands in for the main process API that the reused
// menu modules import.

import { invoke } from '@tauri-apps/api/core'
import { getCurrentWebview } from '@tauri-apps/api/webview'
import { openUrl } from '@tauri-apps/plugin-opener'

type Listener = (event: unknown, ...args: unknown[]) => void
// Accepts handlers with any parameter list; arguments arrive untyped from the preload.
type Handler = (...args: never) => unknown
type UntypedHandler = (...args: unknown[]) => unknown

const listeners = new Map<string, Listener[]>()
// Events emitted before the renderer subscribed to their channel. Electron
// never drops these in practice because main only sends after page load.
const heldEvents = new Map<string, unknown[][]>()
const listenWaiters = new Map<string, Array<() => void>>()

const sendHandlers = new Map<string, Handler>()
const invokeHandlers = new Map<string, Handler>()
const syncHandlers = new Map<string, Handler>()
const reportedMissing = new Set<string>()

// Renderer listeners take `(event, ...args)` and ignore the event.
const rendererEvent = Object.freeze({ sender: null })

const reportMissing = (kind: string, channel: string): void => {
  const key = `${kind} ${channel}`
  if (reportedMissing.has(key)) return
  reportedMissing.add(key)
  console.warn(`[tauri] no ${kind} handler for "${channel}"`)
}

const deliver = (channel: string, args: unknown[]): void => {
  const list = listeners.get(channel)
  if (!list?.length) {
    const held = heldEvents.get(channel) ?? []
    held.push(args)
    heldEvents.set(channel, held)
    return
  }
  for (const listener of [...list]) {
    try {
      listener(rendererEvent, ...args)
    } catch (err) {
      console.error(`[tauri] listener for "${channel}" threw`, err)
    }
  }
}

// Delivered on a later task, as with real IPC, so a reply never runs inside
// the call that asked for it.
export const emitToRenderer = (channel: string, ...args: unknown[]): void => {
  setTimeout(() => deliver(channel, args), 0)
}

export const whenListening = (channel: string): Promise<void> => {
  if (listeners.get(channel)?.length) return Promise.resolve()
  return new Promise((resolve) => {
    const waiters = listenWaiters.get(channel) ?? []
    waiters.push(resolve)
    listenWaiters.set(channel, waiters)
  })
}

const addListener = (channel: string, listener: Listener): void => {
  const list = listeners.get(channel) ?? []
  list.push(listener)
  listeners.set(channel, list)

  const held = heldEvents.get(channel)
  if (held) {
    heldEvents.delete(channel)
    for (const args of held) emitToRenderer(channel, ...args)
  }
  const waiters = listenWaiters.get(channel)
  if (waiters) {
    listenWaiters.delete(channel)
    for (const resolve of waiters) resolve()
  }
}

const removeListener = (channel: string, listener: Listener): void => {
  const list = listeners.get(channel)
  if (!list) return
  const index = list.indexOf(listener)
  if (index !== -1) list.splice(index, 1)
}

export const onSend = (channel: string, handler: Handler): void => {
  sendHandlers.set(channel, handler)
}

export const onInvoke = (channel: string, handler: Handler): void => {
  invokeHandlers.set(channel, handler)
}

export const onSendSync = (channel: string, handler: Handler): void => {
  syncHandlers.set(channel, handler)
}

export const ipcRenderer = {
  send(channel: string, ...args: unknown[]): void {
    const handler = sendHandlers.get(channel)
    if (!handler) return reportMissing('send', channel)
    setTimeout(() => {
      Promise.resolve()
        .then(() => (handler as UntypedHandler)(...args))
        .catch((err) => console.error(`[tauri] "${channel}" failed`, err))
    }, 0)
  },

  async invoke(channel: string, ...args: unknown[]): Promise<unknown> {
    const handler = invokeHandlers.get(channel)
    if (!handler) {
      reportMissing('invoke', channel)
      return undefined
    }
    return (handler as UntypedHandler)(...args)
  },

  sendSync(channel: string, ...args: unknown[]): unknown {
    const handler = syncHandlers.get(channel)
    if (!handler) {
      reportMissing('sendSync', channel)
      return undefined
    }
    return (handler as UntypedHandler)(...args)
  },

  on(channel: string, listener: Listener) {
    addListener(channel, listener)
    return ipcRenderer
  },

  once(channel: string, listener: Listener) {
    const wrapped: Listener = (event, ...args) => {
      removeListener(channel, wrapped)
      listener(event, ...args)
    }
    addListener(channel, wrapped)
    return ipcRenderer
  },

  removeListener(channel: string, listener: Listener) {
    removeListener(channel, listener)
    return ipcRenderer
  },

  removeAllListeners(channel: string) {
    listeners.delete(channel)
    return ipcRenderer
  }
}

export const contextBridge = {
  // No isolated world here: the "preload" shares the page's globals.
  exposeInMainWorld(key: string, api: unknown): void {
    ;(window as unknown as Record<string, unknown>)[key] = api
  }
}

export const webFrame = {
  setZoomFactor(factor: number): void {
    getCurrentWebview()
      .setZoom(factor)
      .catch((err) => console.error('[tauri] setZoom failed', err))
  },
  // Chromium zoom levels are steps of 20 percent.
  setZoomLevel(level: number): void {
    webFrame.setZoomFactor(Math.pow(1.2, level))
  }
}

export const webUtils = {
  // Tauri does not expose file system paths for DOM File objects.
  getPathForFile(): string {
    return ''
  }
}

export type IpcRendererEvent = typeof rendererEvent

// ---- main process side ------------------------------------------------------
// The Electron main process modules that run in the page unchanged (the menu
// templates and most menu actions, see vite.config.ts) import these. Their
// internal events (`ipcMain.emit`) reach the handlers this window registers
// with `ipcMain.on`.

const mainHandlers = new Map<string, Handler>()

export const ipcMain = {
  on(channel: string, handler: Handler) {
    mainHandlers.set(channel, handler)
    return ipcMain
  },

  emit(channel: string, ...args: unknown[]): boolean {
    const handler = mainHandlers.get(channel)
    if (!handler) {
      reportMissing('main event', channel)
      return false
    }
    Promise.resolve()
      .then(() => (handler as UntypedHandler)(...args))
      .catch((err) => console.error(`[tauri] main event "${channel}" failed`, err))
    return true
  }
}

export const app = {
  // Templates pass `app.quit` itself as a click handler, so no `this`.
  quit(): void {
    invoke('quit_app').catch((err) => console.error('[tauri] quit failed', err))
  }
}

export const shell = {
  openExternal: (url: string): Promise<void> => openUrl(url)
}

// Only the macOS branches use these.
export const Menu = {
  sendActionToFirstResponder(): void {},
  getApplicationMenu: (): null => null
}
export const screen = {}

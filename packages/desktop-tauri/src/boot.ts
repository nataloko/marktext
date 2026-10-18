// What Rust tells each window before any page script runs
// (src-tauri/src/main.rs boot_info), and the helpers every module derives from it.

import { getCurrentWindow } from '@tauri-apps/api/window'
import { createPlatformPath } from 'common/filesystem/platformPath'
import type { BootInfo } from '@shared/types/ipc'

interface TauriBootInfo extends BootInfo {
  windowId: number
  windowType: 'editor' | 'settings'
  // The settings page to show first, if any.
  category: string | null
  // Absolute paths to open in this window.
  openFiles: string[]
  // The folder for the sidebar, if any.
  openFolder: string | null
}

declare global {
  interface Window {
    __MT_BOOT__: TauriBootInfo
  }
}

export const boot = window.__MT_BOOT__
export const isWindows = boot.platform === 'win32'
export const path = createPlatformPath(isWindows)
export const appWindow = getCurrentWindow()
export const isSettings = boot.windowType === 'settings'

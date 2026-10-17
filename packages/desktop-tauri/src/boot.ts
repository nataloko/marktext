// What Rust tells each window before any page script runs
// (src-tauri/src/main.rs boot_info), and the helpers every module derives from it.

import { getCurrentWindow } from '@tauri-apps/api/window'
import { createPlatformPath } from 'common/filesystem/platformPath'
import type { BootInfo } from '@shared/types/ipc'

interface TauriBootInfo extends BootInfo {
  windowId: number
  // Absolute paths to open in this window.
  openFiles: string[]
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

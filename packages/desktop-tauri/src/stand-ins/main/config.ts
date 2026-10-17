// Stand-in for packages/desktop/src/main/config.ts, which reads
// `process.platform` while loading, before the preload has defined `process`.

import { boot } from '../../boot'

export const isOsx = boot.platform === 'darwin'
export const isWindows = boot.platform === 'win32'
export const isLinux = boot.platform === 'linux'

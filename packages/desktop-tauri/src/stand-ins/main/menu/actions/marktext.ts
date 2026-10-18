// Stand-in for packages/desktop/src/main/menu/actions/marktext.ts, which sets
// up electron-updater and main process handlers when it loads.

import { openSettings } from '../../../../window'
import { notAvailable } from '../../../notAvailable'

export const userSetting = (): void => {
  void openSettings()
}
export const checkUpdates = (): void => notAvailable('Check for Updates')

// macOS only.
export const osxHide = (): void => {}
export const osxHideAll = (): void => {}
export const osxShowAll = (): void => {}

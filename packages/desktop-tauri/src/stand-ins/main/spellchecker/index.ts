// Stand-in for packages/desktop/src/main/spellchecker/index.ts, which drives
// Electron's session spellchecker. The Tauri port has no spellchecker, and the
// context menu never builds the Spelling submenu that calls this.

export const addToDictionary = (): boolean => false

// Stand-in for packages/desktop/src/common/filesystem/index.ts, which uses
// Node's fs. Only the Help menu imports it, to ask whether the update manifest
// exists; the Tauri trial has no updater, so nothing is a file here.

export const isFile = (): boolean => false

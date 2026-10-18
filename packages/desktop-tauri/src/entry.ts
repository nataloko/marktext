// Boot order matters: the backend answers the preload's synchronous boot info
// request and sets the URL settings, the preload then builds the window.*
// globals, and only then may the renderer load.
import { startBackend } from './backend'
import { loadKeybindings } from './keybindings'

await startBackend()
await import('../../desktop/src/preload/index')
// Needs the preload's `process`, so it cannot run inside startBackend.
await loadKeybindings()
await import('@/main')


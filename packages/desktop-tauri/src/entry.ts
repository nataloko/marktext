// Boot order matters: the backend answers the preload's synchronous boot info
// request and sets the URL settings, the preload then builds the window.*
// globals, and only then may the renderer load.
import { startBackend } from './backend'

await startBackend()
await import('../../desktop/src/preload/index')
await import('@/main')


import { existsSync } from 'fs'
import { resolve, dirname, relative } from 'path'
import { fileURLToPath } from 'url'
import { defineConfig, type PluginOption } from 'vite'
import vue from '@vitejs/plugin-vue'
import svgLoader from 'vite-svg-loader'
import postcssPresetEnv from 'postcss-preset-env'

const __dirname = dirname(fileURLToPath(import.meta.url))
const desktop = resolve(__dirname, '../desktop')
const desktopSrc = resolve(desktop, 'src')
const standIns = resolve(__dirname, 'src/stand-ins')
const isInside = (dir: string, file: string): boolean => !relative(dir, file).startsWith('..')

// The ☰ menu runs Electron's menu templates and actions in the page. Modules
// they import that need Node or Electron resolve to src/stand-ins/<same path>
// instead (src/stand-ins/README.md). Only imports from main process code and
// from this package are checked, so the renderer resolves as before.
const mainStandIns = (): PluginOption => ({
  name: 'marktext-main-stand-ins',
  enforce: 'pre',
  async resolveId(source, importer, options) {
    if (!importer) return null
    const from = importer.split('?')[0]
    if (!isInside(resolve(desktopSrc, 'main'), from) && !isInside(__dirname, from)) return null
    const resolved = await this.resolve(source, importer, { ...options, skipSelf: true })
    if (!resolved || resolved.external) return null
    const target = resolved.id.split('?')[0]
    if (!isInside(desktopSrc, target)) return null
    const standIn = resolve(standIns, relative(desktopSrc, target))
    return existsSync(standIn) ? standIn : null
  }
})

// Mirrors the `renderer` section of packages/desktop/electron.vite.config.ts so
// the unmodified renderer builds the same way. The additions are the `electron`
// alias, which lets the unmodified preload run inside the page, and the main
// process stand-ins for the ☰ menu.
export default defineConfig({
  clearScreen: false,
  assetsInclude: ['**/*.md'],
  define: {
    global: 'globalThis'
  },
  resolve: {
    alias: {
      '@': resolve(desktop, 'src/renderer/src'),
      common: resolve(desktop, 'src/common'),
      '@shared': resolve(desktop, 'src/shared'),
      path: resolve(desktop, 'src/renderer/src/node/platformPath.ts'),
      electron: resolve(__dirname, 'src/electron.ts')
    },
    extensions: ['.mjs', '.ts', '.js', '.json', '.vue']
  },
  optimizeDeps: {
    include: ['pako', 'pathe']
  },
  plugins: [mainStandIns(), vue(), svgLoader()] as PluginOption[],
  css: {
    postcss: {
      plugins: [
        postcssPresetEnv({
          stage: 0,
          features: {
            'nesting-rules': true,
            'logical-properties-and-values': false
          }
        })
      ]
    }
  },
  server: {
    port: 1420,
    strictPort: true,
    fs: {
      allow: [resolve(__dirname, '../..')]
    }
  },
  build: {
    outDir: 'dist',
    emptyOutDir: true,
    // electron-vite's renderer default. The renderer CSS also has an invalid
    // `var(sideBarTitleColor)` that the CSS minifier rejects.
    minify: false,
    modulePreload: { polyfill: false },
    // The entry uses top level await to finish booting before the renderer loads.
    target: 'es2022'
  }
})

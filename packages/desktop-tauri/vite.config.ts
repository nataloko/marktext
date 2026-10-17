import { resolve, dirname } from 'path'
import { fileURLToPath } from 'url'
import { defineConfig, type PluginOption } from 'vite'
import vue from '@vitejs/plugin-vue'
import svgLoader from 'vite-svg-loader'
import postcssPresetEnv from 'postcss-preset-env'

const __dirname = dirname(fileURLToPath(import.meta.url))
const desktop = resolve(__dirname, '../desktop')

// Mirrors the `renderer` section of packages/desktop/electron.vite.config.ts so
// the unmodified renderer builds the same way. The only addition is the
// `electron` alias, which lets the unmodified preload run inside the page.
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
  plugins: [vue(), svgLoader()] as PluginOption[],
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

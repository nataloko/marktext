// Export to HTML and PDF and Print from the renderer's export dialog
// (packages/desktop/src/main/menu/actions/file.ts handleResponseForExport and
// handleResponseForPrint). For PDF and Print the renderer has added a print
// container to the page, and Rust prints the page itself
// (src-tauri/src/print.rs); mt::print-service-clearup removes the container.

import { invoke } from '@tauri-apps/api/core'
import { documentDir } from '@tauri-apps/api/path'
import { save as saveDialog } from '@tauri-apps/plugin-dialog'
import { boot, isWindows, path } from './boot'
import { emitToRenderer, onSend } from './electron'

interface PageOptions {
  pageSize?: string
  pageSizeWidth?: number
  pageSizeHeight?: number
  isLandscape?: boolean
}

interface ExportRequest {
  type: string
  content?: string
  pathname?: string
  title?: string
  pageOptions?: PageOptions
}

const FORMATS: Record<string, { extension: string; filter: { name: string; extensions: string[] } }> = {
  styledHtml: {
    extension: '.html',
    filter: { name: 'Hypertext Markup Language', extensions: ['html'] }
  },
  pdf: {
    extension: '.pdf',
    filter: { name: 'Portable Document Format', extensions: ['pdf'] }
  }
}

const SIDES = ['top', 'right', 'bottom', 'left'] as const
type Margins = Record<(typeof SIDES)[number], number>

const MM_PER_UNIT: Record<string, number> = {
  mm: 1,
  cm: 10,
  q: 0.25,
  in: 25.4,
  pc: 25.4 / 6,
  pt: 25.4 / 72,
  px: 25.4 / 96
}

const toMm = (value: string): number | null => {
  const match = /^(\d*\.?\d+)([a-z]*)$/i.exec(value.trim())
  if (!match) return null
  // Only a zero may go without a unit.
  const factor = match[2] ? MM_PER_UNIT[match[2].toLowerCase()] : Number(match[1]) === 0 ? 1 : undefined
  return factor === undefined ? null : Number(match[1]) * factor
}

// The page margins in the page's print styles, in mm, which WebKitGTK does
// not apply (src-tauri/src/print.rs). The export dialog's rule (renderer
// util/pdf.ts getCssForOptions) comes after the app's default
// (assets/styles/printService.css) and wins, as in the cascade.
const pageMargins = (): Margins => {
  const margins: Margins = { top: 0, right: 0, bottom: 0, left: 0 }
  const visit = (rules: CSSRuleList): void => {
    for (const rule of rules) {
      // Rules for some pages only, such as `@page :first`, are left out.
      if (rule instanceof CSSPageRule) {
        if (rule.selectorText) continue
        for (const side of SIDES) {
          const mm = toMm(rule.style.getPropertyValue(`margin-${side}`))
          if (mm !== null) margins[side] = mm
        }
      } else if (rule instanceof CSSGroupingRule) {
        visit(rule.cssRules)
      }
    }
  }
  for (const sheet of document.styleSheets) visit(sheet.cssRules)
  return margins
}

// file.ts sanitizeFilename.
export const sanitizeFilename = (name?: string): string =>
  (name || '').replace(/[/\\:*?"<>|]/g, '-').trim() || 'Untitled'

export const exportFolder = async(pathname?: string): Promise<string> => {
  if (pathname) return path.dirname(pathname)
  try {
    return await documentDir()
  } catch {
    return boot.env.HOME || ''
  }
}

const exportDocument = async({
  type,
  content,
  pathname,
  title,
  pageOptions
}: ExportRequest): Promise<void> => {
  const format = FORMATS[type]
  if (!format) throw new Error(`Invalid type to export: "${type}".`)
  const { extension, filter } = format
  const name = sanitizeFilename(pathname ? path.basename(pathname, '.md') : title)
  const chosen = await saveDialog({
    defaultPath: path.join(await exportFolder(pathname), `${name}${extension}`),
    filters: [filter]
  })
  if (!chosen) return
  // filesystem/index.ts writeFile adds the extension when it is missing.
  const filePath = chosen.endsWith(extension) ? chosen : `${chosen}${extension}`
  try {
    if (type === 'pdf') {
      await invoke('print_to_pdf', {
        path: filePath,
        options: pageOptions ?? {},
        margins: pageMargins()
      })
    } else {
      if (!content) throw new Error('No HTML content found.')
      await invoke('fs_write_file', { path: filePath, data: content })
    }
    emitToRenderer('mt::export-success', { type, filePath })
  } catch (err) {
    console.error('[export] failed:', err)
    emitToRenderer('mt::show-notification', {
      title: 'Export failure',
      type: 'error',
      message: (err instanceof Error ? err.message : String(err)) || `Error happened when export ${filePath}`
    })
  }
}

// WebView2's window.print() returns once its dialog closes, which the system
// dialog would not tell (src-tauri/src/print.rs start_print).
const printDocument = async(): Promise<void> => {
  if (isWindows) window.print()
  else await invoke('print_page', { margins: pageMargins() })
}

// The renderer's header and footer table (renderer util/exportHtml.ts) sizes
// its cells with 100vw, which WebKitGTK takes from the window when printing
// and Chromium from the page. Wider than the page, the document moved right
// and was cut off.
const WEBKIT_PRINT_STYLE = `@media print {
  table.page-container > tbody,
  table.page-container > tbody > tr,
  table.page-container > tbody > tr > td { width: auto !important; }
}`

export const registerExport = (): void => {
  if (boot.platform === 'linux') {
    const style = document.createElement('style')
    style.textContent = WEBKIT_PRINT_STYLE
    document.head.appendChild(style)
  }
  // Electron leaves the container after a failed PDF export; here it always goes.
  onSend('mt::response-export', async(request: ExportRequest) => {
    try {
      await exportDocument(request)
    } finally {
      if (request.type === 'pdf') emitToRenderer('mt::print-service-clearup')
    }
  })
  onSend('mt::response-print', async() => {
    try {
      await printDocument()
    } catch (err) {
      emitToRenderer('mt::show-notification', {
        title: 'Printing failed',
        type: 'error',
        message: String(err)
      })
    } finally {
      emitToRenderer('mt::print-service-clearup')
    }
  })
}

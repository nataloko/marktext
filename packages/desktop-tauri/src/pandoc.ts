// Importing and exporting with pandoc, standing in for the pandoc parts of
// packages/desktop/src/main/menu/actions/file.ts (importFile, openPandocFile,
// exportWithPandoc, handlePandocExport) and main/ipc/cmd.ts. Rust finds and
// runs pandoc (src-tauri/src/pandoc.rs).

import { invoke } from '@tauri-apps/api/core'
import { open as openDialog, save as saveDialog } from '@tauri-apps/plugin-dialog'
import type { PandocExportPayload } from '@shared/types/files'
import { getCurrentLanguage, t } from '@/i18n'
import { path } from './boot'
import { emitToRenderer, onInvoke, onSend } from './electron'
import { exportFolder, sanitizeFilename } from './export'
import { PANDOC_EXTENSIONS } from './stand-ins/main/config'
import {
  PANDOC_EXPORT_FORMATS,
  formatLinksMedia,
  getPandocLanguage,
  getPandocReader,
  shouldMirrorMedia,
  type PandocReaderExtensions
} from './stand-ins/main/utils/pandoc'

// The pandoc that would run, or null.
export const pandocCommand = (): Promise<string | null> => invoke<string | null>('pandoc_command')

// The renderer offers a link to pandoc.org with this notice.
export const noticePandocNotFound = (titleKey = 'dialog.importWarning'): void => {
  emitToRenderer('mt::pandoc-not-exists', {
    title: t(titleKey),
    type: 'warning',
    message: t('dialog.installPandoc'),
    time: 10000
  })
}

// Notification bodies are HTML and pandoc repeats document text, so escape first.
const escapeNotificationText = (text: string): string =>
  text.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')

const MAX_PANDOC_WARNING_LINES = 5

// The first few lines and a count of the rest: pandoc prints one per link.
const summarizePandocWarnings = (warnings: string): string => {
  const lines = warnings
    .split('\n')
    .map((line) => line.trim())
    .filter((line) => line.length > 0)
  const shown = lines.slice(0, MAX_PANDOC_WARNING_LINES).map(escapeNotificationText)
  const hidden = lines.length - MAX_PANDOC_WARNING_LINES
  if (hidden > 0) {
    shown.push(escapeNotificationText(t('dialog.exportWarningMore', { count: hidden })))
  }
  return shown.join('<br>')
}

// openPandocFile: the document opens as a new untitled tab. Electron opens a
// new window instead when "open files in new window" is on, which the trial's
// windows cannot be given text for yet, and says nothing when pandoc fails.
export const importWithPandoc = async(pathname: string): Promise<void> => {
  try {
    const markdown = await invoke<string>('pandoc_import', { path: pathname })
    emitToRenderer('mt::new-untitled-tab', true, markdown)
  } catch (err) {
    console.error('[pandoc] import failed:', err)
    emitToRenderer('mt::show-notification', {
      title: t('dialog.importWarning'),
      type: 'error',
      message: summarizePandocWarnings(String(err)) || escapeNotificationText(String(err))
    })
  }
}

export const importFile = async(): Promise<void> => {
  if (!(await pandocCommand())) return noticePandocNotFound()
  const picked = await openDialog({
    multiple: false,
    filters: [{ name: 'All Files', extensions: [...PANDOC_EXTENSIONS] }]
  })
  if (typeof picked === 'string') await importWithPandoc(picked)
}

// The renderer answers with mt::response-pandoc-export.
export const exportWithPandoc = async(id: string): Promise<void> => {
  if (!(await pandocCommand())) return noticePandocNotFound('dialog.exportWarning')
  emitToRenderer('mt::export-with-pandoc', id)
}

interface Extension {
  name: string
  enabled: boolean
}

const readerExtensions = async(): Promise<PandocReaderExtensions | null> => {
  const listed = await invoke<{ gfm: Extension[]; markdown: Extension[] } | null>(
    'pandoc_reader_extensions'
  )
  const toMap = (list: Extension[]): Map<string, boolean> =>
    new Map(list.map(({ name, enabled }) => [name, enabled]))
  return listed && { gfm: toMap(listed.gfm), markdown: toMap(listed.markdown) }
}

// handlePandocExport.
const exportDocument = async(payload: PandocExportPayload): Promise<void> => {
  const format = PANDOC_EXPORT_FORMATS.find((f) => f.id === payload.target)
  if (!format) {
    console.error(`[pandoc] unknown export target: ${payload.target}`)
    return
  }
  const { markdown, title, pathname } = payload
  // The save dialog and pandoc's links both start from the document's folder.
  const sourceDir = pathname ? path.dirname(pathname) : undefined
  const stem = sanitizeFilename(pathname ? path.basename(pathname, path.extname(pathname)) : title)
  try {
    const filePath = await saveDialog({
      defaultPath: path.join(sourceDir ?? (await exportFolder()), `${stem}${format.extension}`),
      filters: [{ name: format.label, extensions: [format.extension.slice(1)] }]
    })
    if (!filePath) return
    const reader = getPandocReader(
      {
        superSubScript: payload.superSubScript === true,
        footnotes: payload.footnote === true,
        texMathDollars: payload.texMathDollars === true,
        texMathGfm: payload.texMathGfm === true,
        texMathSingleBackslash: payload.texMathSingleBackslash === true,
        texMathDoubleBackslash: payload.texMathDoubleBackslash === true
      },
      await readerExtensions()
    )
    const linksMedia = formatLinksMedia(format.target)
    const media = linksMedia
      ? await invoke<string[]>('pandoc_linked_media', { markdown, reader })
      : []
    const warnings = await invoke<string>('pandoc_export', {
      request: {
        target: format.target,
        outputPath: filePath,
        markdown,
        reader,
        // Only EPUB takes a title; `lang` is the app's language.
        metadata: {
          ...(format.target === 'epub3' ? { title: title || stem } : {}),
          lang: getPandocLanguage(getCurrentLanguage())
        },
        cwd: sourceDir ?? null,
        mirrorMedia: linksMedia && shouldMirrorMedia(media, sourceDir, filePath),
        resourcePath: sourceDir ?? null
      }
    })
    // A finished conversion can still have dropped an image it could not read.
    const message = summarizePandocWarnings(warnings)
    if (message) {
      console.warn(`[pandoc] export warnings for ${filePath}:`, warnings)
      emitToRenderer('mt::show-notification', { title: t('dialog.exportWarning'), type: 'warning', message })
    }
    emitToRenderer('mt::export-success', { type: format.id, filePath, linksMedia })
  } catch (err) {
    console.error('[pandoc] export failed:', err)
    const text = String(err)
    emitToRenderer('mt::show-notification', {
      title: t('editor.export.failed', { type: format.label }),
      type: 'error',
      message: summarizePandocWarnings(text) || escapeNotificationText(text)
    })
  }
}

export const registerPandoc = (): void => {
  onSend('mt::cmd-import-file', () => importFile())
  onSend('mt::response-pandoc-export', exportDocument)
  // cmd.ts: the General page shows which pandoc it found.
  onInvoke('mt::pandoc::command', async() => ({ command: await pandocCommand() }))
}

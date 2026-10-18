// Stand-in for packages/desktop/src/main/utils/pandoc.ts, which runs pandoc
// with child_process; here Rust runs it (src-tauri/src/pandoc.rs). This keeps
// the original's export formats, which the File menu builds from, and the
// helpers that decide how a document is read and whether its images travel
// with it. Keep both in step with the original.

import { path } from '../../../boot'

interface PandocExportFormat {
  id: string
  label: string
  target: string
  extension: string
}

export const PANDOC_EXPORT_FORMATS: readonly PandocExportFormat[] = Object.freeze([
  { id: 'docx', label: 'Word (.docx)', target: 'docx', extension: '.docx' },
  { id: 'odt', label: 'OpenDocument (.odt)', target: 'odt', extension: '.odt' },
  { id: 'rtf', label: 'RTF (.rtf)', target: 'rtf', extension: '.rtf' },
  { id: 'epub', label: 'EPUB (.epub)', target: 'epub3', extension: '.epub' },
  { id: 'latex', label: 'LaTeX (.tex)', target: 'latex', extension: '.tex' },
  { id: 'rst', label: 'reStructuredText (.rst)', target: 'rst', extension: '.rst' },
  { id: 'org', label: 'Org mode (.org)', target: 'org', extension: '.org' },
  { id: 'mediawiki', label: 'MediaWiki (.wiki)', target: 'mediawiki', extension: '.wiki' },
  { id: 'textile', label: 'Textile (.textile)', target: 'textile', extension: '.textile' }
])

// These writers keep images as links, so the pictures are copied along.
export const formatLinksMedia = (target: string): boolean =>
  ['latex', 'rst', 'org', 'mediawiki', 'textile'].includes(target)

export const isRemoteMedia = (url: string): boolean => /^(https?:)?\/\//i.test(url)

// Whether the export copies the document's pictures next to it
// (`--extract-media`), which replaces a link it cannot read with the alt text:
// only when every link resolves (no remote ones; relative ones need the saved
// document's folder) and the output is in another folder.
export const shouldMirrorMedia = (
  links: string[],
  sourceDir: string | undefined,
  outputPath: string
): boolean =>
  !links.some(isRemoteMedia) &&
  (!!sourceDir || links.every((url) => path.isAbsolute(url))) &&
  (sourceDir === undefined || path.relative(sourceDir, path.dirname(outputPath)) !== '')

// The Preferences → Markdown switches that map onto pandoc reader extensions.
export interface PandocReaderOptions {
  superSubScript: boolean
  footnotes?: boolean
  texMathDollars?: boolean
  texMathGfm?: boolean
  texMathSingleBackslash?: boolean
  texMathDoubleBackslash?: boolean
}

// Each extension a reader lists, and whether the reader has it on by default.
export interface PandocReaderExtensions {
  gfm: ReadonlyMap<string, boolean>
  markdown: ReadonlyMap<string, boolean>
}

// The reader an export parses with (see the original for the reasons): gfm,
// unless backslash math is on, which pandoc's gfm lacks; then markdown with
// every other extension set to gfm's default. Extensions are named only from
// the installed pandoc's own listing, as pandoc rejects a reader naming one
// it does not know.
export const getPandocReader = (
  options: PandocReaderOptions,
  extensions?: PandocReaderExtensions | null
): string => {
  const {
    superSubScript,
    footnotes = true,
    texMathDollars = true,
    texMathGfm = false,
    texMathSingleBackslash = false,
    texMathDoubleBackslash = false
  } = options

  if (texMathSingleBackslash || texMathDoubleBackslash) {
    const desired = (name: string): boolean => {
      switch (name) {
        case 'superscript':
        case 'subscript':
          return superSubScript
        case 'footnotes':
          return footnotes
        case 'tex_math_dollars':
          return texMathDollars
        case 'tex_math_single_backslash':
          return texMathSingleBackslash
        case 'tex_math_double_backslash':
          return texMathDoubleBackslash
        default:
          return extensions?.gfm.get(name) ?? false
      }
    }
    const flags: string[] = []
    for (const [name, markdownDefault] of extensions?.markdown ?? []) {
      const want = desired(name)
      if (want && !markdownDefault) flags.push(`+${name}`)
      if (!want && markdownDefault) flags.push(`-${name}`)
    }
    if (!extensions) {
      if (!texMathDollars) flags.push('-tex_math_dollars')
      if (texMathSingleBackslash) flags.push('+tex_math_single_backslash')
      if (texMathDoubleBackslash) flags.push('+tex_math_double_backslash')
    }
    return `markdown${flags.join('')}`
  }

  const flags: string[] = []
  if (superSubScript) flags.push('+superscript+subscript')
  if (!footnotes) flags.push('-footnotes')
  if (!texMathDollars) flags.push('-tex_math_dollars')
  if (extensions?.gfm.has('tex_math_gfm')) {
    flags.push(texMathGfm ? '+tex_math_gfm' : '-tex_math_gfm')
  }
  return `gfm${flags.join('')}`
}

// pandoc knows no `zh` or `zh-CN` and would warn, so Chinese takes the script subtag.
export const getPandocLanguage = (locale: string): string => {
  const [language, region = ''] = locale.trim().split(/[-_]/)
  if (language?.toLowerCase() !== 'zh') return locale
  return /^(hant|tw|hk|mo)$/i.test(region) ? 'zh-Hant' : 'zh-Hans'
}

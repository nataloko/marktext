// loadMarkdownFile / writeMarkdownFile from
// packages/desktop/src/main/filesystem/markdown.ts. Rust reads and writes the
// bytes and handles the encoding (src-tauri/src/encoding.rs); this part does
// the line endings and the trailing newline.

import { invoke } from '@tauri-apps/api/core'

type LineEnding = 'lf' | 'crlf'

export interface MarkdownOptions {
  encoding: { encoding: string; isBom: boolean }
  lineEnding: LineEnding
  adjustLineEndingOnSave: boolean
}

export interface LoadPreferences {
  preferredEol: LineEnding
  autoGuessEncoding: boolean
  trimTrailingNewline: number
  autoNormalizeLineEndings: boolean
}

interface LoadedText {
  text: string
  encoding: string
  isBom: boolean
}

// Same expressions as packages/desktop/src/main/config.ts.
const LINE_ENDING_REG = /(?:\r\n|\n)/g
const LF_LINE_ENDING_REG = /(?:[^\r]\n)|(?:^\n$)/
const CRLF_LINE_ENDING_REG = /\r\n/

const convertLineEndings = (text: string, lineEnding: LineEnding): string =>
  text.replace(LINE_ENDING_REG, lineEnding === 'crlf' ? '\r\n' : '\n')

const basename = (pathname: string): string => pathname.split(/[\\/]/).pop() ?? pathname

export const loadMarkdownFile = async(pathname: string, prefs: LoadPreferences) => {
  const loaded = await invoke<LoadedText>('read_markdown_file', {
    path: pathname,
    autoGuessEncoding: prefs.autoGuessEncoding
  })
  let markdown = loaded.text

  const isLf = LF_LINE_ENDING_REG.test(markdown)
  const isCrlf = CRLF_LINE_ENDING_REG.test(markdown)
  const isMixedLineEndings = isLf && isCrlf
  const isUnknownEnding = !isLf && !isCrlf
  let lineEnding: LineEnding = prefs.preferredEol
  if (isLf && !isCrlf) lineEnding = 'lf'
  else if (isCrlf && !isLf) lineEnding = 'crlf'

  let adjustLineEndingOnSave = false
  if (isMixedLineEndings || isUnknownEnding || lineEnding !== 'lf') {
    markdown = convertLineEndings(markdown, 'lf')
    adjustLineEndingOnSave = !prefs.autoNormalizeLineEndings && lineEnding !== 'lf'
  }

  // 0 = no final newline, 1 = one, 2 = two or more, 3 = empty file.
  let trimTrailingNewline = prefs.trimTrailingNewline
  if (trimTrailingNewline === 2) {
    const last = markdown.length - 1
    if (!markdown) trimTrailingNewline = 3
    else if (last >= 1 && markdown[last] === '\n' && markdown[last - 1] === '\n')
      trimTrailingNewline = 2
    else if (markdown[last] === '\n') trimTrailingNewline = 1
    else trimTrailingNewline = 0
  }

  return {
    markdown,
    filename: basename(pathname),
    pathname,
    encoding: { encoding: loaded.encoding, isBom: loaded.isBom },
    lineEnding,
    adjustLineEndingOnSave,
    trimTrailingNewline,
    isMixedLineEndings
  }
}

// filesystem/markdown.ts loadCliMarkdownFile: the empty document for a file
// named on the command line that does not exist yet.
export const pendingMarkdownFile = (
  pathname: string,
  prefs: LoadPreferences,
  defaultEncoding: string
) => ({
  markdown: '',
  filename: basename(pathname),
  pathname,
  isPendingCreation: true,
  encoding: { encoding: defaultEncoding, isBom: false },
  lineEnding: prefs.preferredEol,
  adjustLineEndingOnSave: prefs.preferredEol !== 'lf',
  trimTrailingNewline: prefs.trimTrailingNewline,
  isMixedLineEndings: false
})

export const writeMarkdownFile = async(
  pathname: string,
  content: string,
  options: MarkdownOptions
): Promise<void> => {
  const { encoding, isBom } = options.encoding
  if (options.adjustLineEndingOnSave) {
    content = convertLineEndings(content, options.lineEnding)
  }
  await invoke('write_markdown_file', { path: pathname, text: content, encoding, isBom })
}

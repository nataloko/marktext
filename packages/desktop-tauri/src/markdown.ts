// Trial versions of loadMarkdownFile / writeMarkdownFile from
// packages/desktop/src/main/filesystem/markdown.ts. UTF-8 only: the Rust side
// refuses other encodings rather than guessing, so a save can never re-encode
// a file wrongly.

import { invoke } from '@tauri-apps/api/core'

type LineEnding = 'lf' | 'crlf'

export interface MarkdownOptions {
  encoding: { encoding: string; isBom: boolean }
  lineEnding: LineEnding
  adjustLineEndingOnSave: boolean
}

export interface LoadPreferences {
  preferredEol: LineEnding
  trimTrailingNewline: number
  autoNormalizeLineEndings: boolean
}

// Same expressions as packages/desktop/src/main/config.ts.
const LINE_ENDING_REG = /(?:\r\n|\n)/g
const LF_LINE_ENDING_REG = /(?:[^\r]\n)|(?:^\n$)/
const CRLF_LINE_ENDING_REG = /\r\n/
const BOM = '﻿'

const convertLineEndings = (text: string, lineEnding: LineEnding): string =>
  text.replace(LINE_ENDING_REG, lineEnding === 'crlf' ? '\r\n' : '\n')

const basename = (pathname: string): string => pathname.split(/[\\/]/).pop() ?? pathname

export const loadMarkdownFile = async(pathname: string, prefs: LoadPreferences) => {
  let markdown = await invoke<string>('read_text_file', { path: pathname })

  const isBom = markdown.startsWith(BOM)
  if (isBom) markdown = markdown.slice(1)

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
    encoding: { encoding: 'utf8', isBom },
    lineEnding,
    adjustLineEndingOnSave,
    trimTrailingNewline,
    isMixedLineEndings
  }
}

export const writeMarkdownFile = async(
  pathname: string,
  content: string,
  options: MarkdownOptions
): Promise<void> => {
  const { encoding, isBom } = options.encoding
  if (!/^utf-?8$/i.test(encoding)) {
    throw new Error(`Saving as "${encoding}" is not supported in the Tauri trial.`)
  }
  if (options.adjustLineEndingOnSave) {
    content = convertLineEndings(content, options.lineEnding)
  }
  await invoke('write_text_file', { path: pathname, contents: isBom ? BOM + content : content })
}

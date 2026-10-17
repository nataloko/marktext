// Stand-in for packages/desktop/src/main/utils/pandoc.ts, which runs pandoc
// with child_process. The File menu only needs the export formats; keep this
// list in step with the original.

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

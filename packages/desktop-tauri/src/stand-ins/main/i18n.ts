// Stand-in for packages/desktop/src/main/i18n.ts, which reads the locale files
// with Node. The menu uses the renderer's translations instead, so it follows
// the renderer's language.

export { t, getCurrentLanguage } from '@/i18n'

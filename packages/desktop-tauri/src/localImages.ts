// Local images. Muya and the renderer point images at `file://` addresses, as
// Electron's page may load them, but the webviews refuse those from the app's
// own page ("Not allowed to load local resource"). Each such address is
// rewritten to Tauri's asset protocol (tauri.conf.json
// app.security.assetProtocol) as it is set, and images that arrive in HTML
// (Muya re-renders blocks through innerHTML) once they are in the document.

import { convertFileSrc } from '@tauri-apps/api/core'

const FILE_URL = /^file:\/\//i

// The query is kept: Muya adds one so that a changed image is read afresh.
export const toAssetUrl = (value: string): string => {
  if (!FILE_URL.test(value)) return value
  let url: URL
  try {
    url = new URL(value)
  } catch {
    return value
  }
  let path = decodeURIComponent(url.pathname)
  // file:///C:/notes/a.png on Windows.
  if (/^\/[a-z]:\//i.test(path)) path = path.slice(1)
  // A share, file://server/share/a.png.
  if (url.host) path = `//${url.host}${path}`
  return convertFileSrc(path) + url.search
}

const rewrite = (image: HTMLImageElement): void => {
  const value = image.getAttribute('src')
  // The setAttribute hook below does the rewriting.
  if (value && FILE_URL.test(value)) image.setAttribute('src', value)
}

export const installLocalImages = (): void => {
  const src = Object.getOwnPropertyDescriptor(HTMLImageElement.prototype, 'src')
  const setSrc = src?.set
  if (src && setSrc) {
    Object.defineProperty(HTMLImageElement.prototype, 'src', {
      ...src,
      set(this: HTMLImageElement, value: string) {
        setSrc.call(this, toAssetUrl(String(value)))
      }
    })
  }

  const setAttribute = Element.prototype.setAttribute
  Element.prototype.setAttribute = function(this: Element, name: string, value: string) {
    const isImageSource = this instanceof HTMLImageElement && name.toLowerCase() === 'src'
    setAttribute.call(this, name, isImageSource ? toAssetUrl(String(value)) : value)
  }

  new MutationObserver((records) => {
    for (const record of records) {
      if (record.target instanceof HTMLImageElement) rewrite(record.target)
      for (const node of record.addedNodes) {
        if (node instanceof HTMLImageElement) rewrite(node)
        else if (node instanceof Element) node.querySelectorAll('img').forEach(rewrite)
      }
    }
  }).observe(document, { subtree: true, childList: true, attributes: true, attributeFilter: ['src'] })
}

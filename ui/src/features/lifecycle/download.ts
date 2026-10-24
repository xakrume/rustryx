/**
 * Hand a text to the browser as a downloaded file. The object URL is created, clicked and
 * revoked in one go. A `download` link is a navigation, not a fetch, so the strict
 * Content-Security-Policy (no `blob:` in any fetch directive) does not apply to it.
 */
export function downloadTextFile(filename: string, content: string, type = 'text/csv'): void {
  const url = URL.createObjectURL(new Blob([content], { type: `${type};charset=utf-8` }))
  const link = document.createElement('a')
  link.href = url
  link.download = filename
  link.rel = 'noopener'
  document.body.append(link)
  link.click()
  link.remove()
  window.setTimeout(() => URL.revokeObjectURL(url), 0)
}

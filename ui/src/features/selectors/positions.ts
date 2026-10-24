const utf8Size = (codePoint: number): number =>
  codePoint < 0x80 ? 1 : codePoint < 0x800 ? 2 : codePoint < 0x10000 ? 3 : 4

/**
 * The compile errors of the API carry a BYTE offset into the UTF-8 expression, but a text box
 * counts UTF-16 units. This converts one into the other (an offset inside a multi byte character
 * lands on that character), clamped to the text.
 */
export function byteOffsetToIndex(text: string, byteOffset: number): number {
  let bytes = 0
  let index = 0
  for (const char of text) {
    const size = utf8Size(char.codePointAt(0) ?? 0)
    if (bytes + size > byteOffset) return index
    bytes += size
    index += char.length
  }
  return index
}

export interface Location {
  /** UTF-16 index into the text, for `setSelectionRange`. */
  index: number
  /** One based, for people. */
  line: number
  column: number
  /** The text of that line, for the code frame. */
  lineText: string
}

/** Where a byte offset is, as line and column, or null when the error has no position. */
export function locate(text: string, byteOffset: number | null): Location | null {
  if (byteOffset === null) return null
  const index = byteOffsetToIndex(text, byteOffset)
  const before = text.slice(0, index)
  const line = before.split('\n').length
  const lineStart = before.lastIndexOf('\n') + 1
  const lineEnd = text.indexOf('\n', index)
  return {
    index,
    line,
    column: [...before.slice(lineStart)].length + 1,
    lineText: text.slice(lineStart, lineEnd === -1 ? text.length : lineEnd),
  }
}

/** UTF-8 size of a string, the unit of the 1024 byte limit of an expression. */
export const byteLength = (text: string): number => new TextEncoder().encode(text).length

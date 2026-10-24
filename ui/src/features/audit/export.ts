import { api } from '@/api/client'
import { toApiError, networkError, isAbortError } from '@/api/errors'
import { auditQuery, type AuditFilters, type TimeBounds } from './api'

export interface ExportProgress {
  /** Complete events written so far, across resumes. */
  count: number
  /** Cursor of the last complete event; `after` of a resume. */
  cursor: string | null
}

const NEWLINE = 0x0a
const CURSOR = /"cursor":"([^"]+)"\}\s*$/

/**
 * The export is a stream of JSON lines. This transform lets only COMPLETE lines through (a line
 * is complete when its newline arrived) and reads the `cursor` of each, so whatever reaches the
 * file is always whole events, a cut connection leaves a clean file, and a resume can continue
 * exactly after the last complete event (`after=<cursor>`, api-conventions 15).
 */
export function lineGate(
  start: ExportProgress,
  onProgress: (progress: ExportProgress) => void,
): TransformStream<Uint8Array, Uint8Array> {
  let pending = new Uint8Array(0)
  let progress = start
  const decoder = new TextDecoder()
  return new TransformStream({
    transform(chunk, controller) {
      const joined = new Uint8Array(pending.length + chunk.length)
      joined.set(pending)
      joined.set(chunk, pending.length)
      const last = joined.lastIndexOf(NEWLINE)
      if (last === -1) {
        pending = joined
        return
      }
      const complete = joined.subarray(0, last + 1)
      pending = joined.slice(last + 1)
      controller.enqueue(complete.slice())
      let count = progress.count
      let cursor = progress.cursor
      for (const line of decoder.decode(complete).split('\n')) {
        if (line === '') continue
        count += 1
        cursor = CURSOR.exec(line)?.[1] ?? cursor
      }
      progress = { count, cursor }
      onProgress(progress)
    },
    flush() {
      if (pending.length > 0) throw new Error('The export ended inside an event')
    },
  })
}

/** Where the bytes go: a file on disk when the browser can stream to one, else memory. */
export interface ExportSink {
  write: (stream: ReadableStream<Uint8Array>, signal: AbortSignal) => Promise<void>
  /** Completes the file (or hands the memory copy to the browser as a download). */
  finish: (fileName: string) => Promise<void>
  /** Gives up: the file on disk is aborted, memory is dropped. */
  abort: () => Promise<void>
}

interface FileHandleLike {
  createWritable: () => Promise<
    WritableStream<Uint8Array> & { close: () => Promise<void>; abort: () => Promise<void> }
  >
}
type SavePicker = (options: {
  suggestedName: string
  types: { description: string; accept: Record<string, string[]> }[]
}) => Promise<FileHandleLike>

export class SaveDeclined extends Error {
  constructor() {
    super('The save dialog was closed')
    this.name = 'SaveDeclined'
  }
}

/**
 * Streams to a file the user picked (the File System Access API, Chromium) without holding the
 * export in memory, so a very large range costs no RAM. Elsewhere it collects the complete lines
 * in memory and offers a download at the end.
 */
export async function openSink(suggestedName: string): Promise<ExportSink> {
  const picker = (window as unknown as { showSaveFilePicker?: SavePicker }).showSaveFilePicker
  if (typeof picker === 'function') {
    let handle: FileHandleLike
    try {
      handle = await picker.call(window, {
        suggestedName,
        types: [
          {
            description: 'Newline delimited JSON',
            accept: { 'application/x-ndjson': ['.ndjson'] },
          },
        ],
      })
    } catch (error) {
      if (isAbortError(error)) throw new SaveDeclined()
      throw error
    }
    const writable = await handle.createWritable()
    return {
      // The writable stays open when the network stream breaks, so a resume appends to the same file.
      write: (stream, signal) =>
        stream.pipeTo(writable, { preventClose: true, preventAbort: true, signal }),
      finish: () => writable.close(),
      abort: () => writable.abort(),
    }
  }
  const parts: Uint8Array[] = []
  return {
    write: async (stream, signal) => {
      const reader = stream.getReader()
      try {
        for (;;) {
          if (signal.aborted) throw new DOMException('Aborted', 'AbortError')
          const { done, value } = await reader.read()
          if (done) return
          parts.push(value)
        }
      } finally {
        reader.releaseLock()
      }
    },
    finish: (fileName) => {
      const url = URL.createObjectURL(
        new Blob(parts as BlobPart[], { type: 'application/x-ndjson' }),
      )
      const link = document.createElement('a')
      link.href = url
      link.download = fileName
      link.rel = 'noopener'
      document.body.appendChild(link)
      link.click()
      link.remove()
      window.setTimeout(() => URL.revokeObjectURL(url), 10_000)
      return Promise.resolve()
    },
    abort: () => {
      parts.length = 0
      return Promise.resolve()
    },
  }
}

export interface ExportRequest {
  filters: AuditFilters
  bounds: TimeBounds
  after: string | null
  signal: AbortSignal
}

/** Opens the export response as a byte stream; a refusal (400, 403, 429, 503) is an ApiError. */
export async function openExportStream(request: ExportRequest): Promise<{
  stream: ReadableStream<Uint8Array>
  fileName: string | null
}> {
  const query = {
    ...auditQuery(request.filters, request.bounds),
    after: request.after ?? undefined,
  }
  const result = await api
    .GET('/api/v1/audit-events/export', {
      params: { query },
      parseAs: 'stream',
      signal: request.signal,
    })
    .catch((error: unknown) => {
      if (isAbortError(error)) throw error
      throw networkError(error)
    })
  if (!result.response.ok) throw toApiError(result.response, result.error)
  const body = result.response.body
  if (body === null) throw networkError(new Error('The export has no body'))
  const disposition = result.response.headers.get('content-disposition') ?? ''
  return { stream: body, fileName: /filename="?([^";]+)"?/.exec(disposition)?.[1] ?? null }
}

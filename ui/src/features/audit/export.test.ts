import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { isApiError } from '@/api/errors'
import { readBlob, useIdentityMock } from '@/features/roles/test-support'
import { lineGate, openExportStream, openSink, SaveDeclined, type ExportProgress } from './export'

const encoder = new TextEncoder()
const line = (n: number): string => `{"id":"e${n}","cursor":"c${n}"}\n`

function streamOf(...chunks: string[]): ReadableStream<Uint8Array> {
  return new ReadableStream({
    start(controller) {
      for (const chunk of chunks) controller.enqueue(encoder.encode(chunk))
      controller.close()
    },
  })
}

async function drain(stream: ReadableStream<Uint8Array>): Promise<string> {
  const reader = stream.getReader()
  const decoder = new TextDecoder()
  let text = ''
  for (;;) {
    const { done, value } = await reader.read()
    if (done) return text
    text += decoder.decode(value, { stream: true })
  }
}

describe('lineGate', () => {
  it('lets only complete lines through, even when a chunk ends in the middle of one', async () => {
    const seen: ExportProgress[] = []
    const gated = streamOf(line(1) + '{"id":"e2","cur', 'sor":"c2"}\n' + line(3)).pipeThrough(
      lineGate({ count: 0, cursor: null }, (p) => seen.push(p)),
    )
    expect(await drain(gated)).toBe(line(1) + line(2) + line(3))
    expect(seen.at(-1)).toEqual({ count: 3, cursor: 'c3' })
    expect(seen[0]).toEqual({ count: 1, cursor: 'c1' })
  })

  it('counts on from a resume and keeps the cursor of the last complete line', async () => {
    const gated = streamOf(line(7) + line(8)).pipeThrough(
      lineGate({ count: 5, cursor: 'c5' }, () => undefined),
    )
    await drain(gated)
    const again = streamOf(line(9)).pipeThrough(
      lineGate({ count: 7, cursor: 'c8' }, () => undefined),
    )
    expect(await drain(again)).toBe(line(9))
  })

  it('fails when the stream ends inside an event, so a cut export is never taken for a whole one', async () => {
    const gated = streamOf(line(1) + '{"id":"e2","cur').pipeThrough(
      lineGate({ count: 0, cursor: null }, () => undefined),
    )
    await expect(drain(gated)).rejects.toThrow('inside an event')
  })

  it('passes nothing for a chunk without a newline and ignores empty lines', async () => {
    const gated = streamOf('{"id":', '"e1","cursor":"c1"}\n\n').pipeThrough(
      lineGate({ count: 0, cursor: null }, () => undefined),
    )
    expect(await drain(gated)).toBe('{"id":"e1","cursor":"c1"}\n\n')
  })
})

describe('openSink', () => {
  afterEach(() => {
    delete (window as { showSaveFilePicker?: unknown }).showSaveFilePicker
    vi.unstubAllGlobals()
  })

  it('collects in memory and offers a download when the browser cannot write to a file', async () => {
    const created: Blob[] = []
    URL.createObjectURL = vi.fn((blob: Blob | MediaSource) => {
      created.push(blob as Blob)
      return 'blob:test'
    })
    URL.revokeObjectURL = vi.fn()
    const clicks: string[] = []
    const original = HTMLAnchorElement.prototype.click
    HTMLAnchorElement.prototype.click = function click(this: HTMLAnchorElement) {
      clicks.push(this.download)
    }
    try {
      const sink = await openSink('x.ndjson')
      await sink.write(streamOf(line(1), line(2)), new AbortController().signal)
      await sink.finish('audit.ndjson')
      expect(clicks).toEqual(['audit.ndjson'])
      expect(await readBlob(created[0]!)).toBe(line(1) + line(2))
      expect(created[0]?.type).toBe('application/x-ndjson')
    } finally {
      HTMLAnchorElement.prototype.click = original
    }
  })

  it('drops the memory copy on abort and stops writing when the signal is aborted', async () => {
    const sink = await openSink('x.ndjson')
    const controller = new AbortController()
    controller.abort()
    await expect(sink.write(streamOf(line(1)), controller.signal)).rejects.toThrow()
    await sink.abort()
  })

  it('streams to a file the user picked, without holding the export in memory', async () => {
    const written: string[] = []
    let closed = false
    const writable = Object.assign(
      new WritableStream<Uint8Array>({
        write: (chunk) => {
          written.push(new TextDecoder().decode(chunk))
        },
      }),
      {
        close: () => {
          closed = true
          return Promise.resolve()
        },
        abort: () => Promise.resolve(),
      },
    )
    ;(window as { showSaveFilePicker?: unknown }).showSaveFilePicker = vi.fn(() =>
      Promise.resolve({ createWritable: () => Promise.resolve(writable) }),
    )
    const sink = await openSink('x.ndjson')
    await sink.write(streamOf(line(1), line(2)), new AbortController().signal)
    expect(written.join('')).toBe(line(1) + line(2))
    expect(closed).toBe(false)
    await sink.finish('x.ndjson')
    expect(closed).toBe(true)
    await sink.abort()
  })

  it('reports a closed save dialog as a declined save, and passes other failures on', async () => {
    ;(window as { showSaveFilePicker?: unknown }).showSaveFilePicker = vi.fn(() =>
      Promise.reject(new DOMException('closed', 'AbortError')),
    )
    await expect(openSink('x')).rejects.toBeInstanceOf(SaveDeclined)
    ;(window as { showSaveFilePicker?: unknown }).showSaveFilePicker = vi.fn(() =>
      Promise.reject(new Error('disk')),
    )
    await expect(openSink('x')).rejects.toThrow('disk')
  })
})

describe('openExportStream', () => {
  beforeEach(() => useIdentityMock())

  it('opens the stream with the filters, the range and the cursor, and reads the file name', async () => {
    const opened = await openExportStream({
      filters: { outcome: 'failure' },
      bounds: {},
      after: null,
      signal: new AbortController().signal,
    })
    expect(opened.fileName).toBe('audit-events.ndjson')
    const text = await drain(opened.stream)
    expect(text.endsWith('\n')).toBe(true)
    expect(
      text
        .split('\n')
        .filter(Boolean)
        .every((l) => {
          const parsed: unknown = JSON.parse(l)
          return (
            typeof parsed === 'object' &&
            parsed !== null &&
            'outcome' in parsed &&
            parsed.outcome === 'failure'
          )
        }),
    ).toBe(true)
  })

  it('turns a refusal into an API error with the code and details', async () => {
    const failure = await openExportStream({
      filters: {},
      bounds: { from: '2024-01-01T00:00:00Z', to: '2026-01-01T00:00:00Z' },
      after: null,
      signal: new AbortController().signal,
    }).catch((error: unknown) => error)
    expect(isApiError(failure)).toBe(true)
    expect(isApiError(failure) && failure.code).toBe('range_too_large')
    expect(isApiError(failure) && failure.details?.max_days).toBe(366)
  })
})

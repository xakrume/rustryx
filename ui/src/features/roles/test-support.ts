import { setCsrfToken } from '@/api/csrf'
import { handlers } from '@/mocks/handlers'
import { startSession } from '@/mocks/session'
import { repositories } from '@/mocks/store'
import { emptyJson, parseJson, type ApiJson } from '@/test/api-json'
import { server } from '@/test/server'
import { resetIdentity } from './mock-store'

export const BASE = 'http://localhost:3000'

export interface Reply {
  status: number
  json: ApiJson
  headers: Headers
}

let csrf = ''

/** Fresh identity data, the mock handlers on the test server, and a signed in mock account. */
export function useIdentityMock(user: 'admin' | 'alice' = 'admin'): void {
  resetIdentity()
  server.use(...handlers)
  const session = startSession(user)
  csrf = session.csrf
  setCsrfToken(session.csrf)
}

/** Restores the shared repository list the anonymous flags change, so tests stay independent. */
export function protectRepositories(): () => void {
  const snapshot = [...repositories]
  return () => {
    repositories.splice(0, repositories.length, ...snapshot)
  }
}

/** One call to the mock API with the session and CSRF header a browser would send. */
export async function call(
  method: string,
  path: string,
  options: { body?: unknown; headers?: Record<string, string> } = {},
): Promise<Reply> {
  const response = await fetch(`${BASE}${path}`, {
    method,
    headers: { 'content-type': 'application/json', 'x-csrf-token': csrf, ...options.headers },
    body: options.body === undefined ? undefined : JSON.stringify(options.body),
  })
  const text = await response.text()
  return {
    status: response.status,
    json: text === '' ? emptyJson() : parseJson(text),
    headers: response.headers,
  }
}

/** GET an object, then PUT or PATCH it back with its ETag, changed by `change`. */
export async function readEtag(path: string): Promise<{ etag: string; json: Reply['json'] }> {
  const reply = await call('GET', path)
  return { etag: reply.headers.get('etag') ?? '', json: reply.json }
}

export const idOf = (name: string, items: { id: string; name: string }[]): string =>
  items.find((i) => i.name === name)?.id ?? ''

/** jsdom's Blob has no `text()`: read it the old way. */
export function readBlob(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader()
    reader.onload = () => {
      const result = reader.result
      // readAsText yields a string. Anything else is not the file contents.
      resolve(typeof result === 'string' ? result : '')
    }
    reader.onerror = () => reject(reader.error ?? new Error('Could not read the blob'))
    reader.readAsText(blob)
  })
}

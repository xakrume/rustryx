/** A scripted XMLHttpRequest: the test decides what the network does. */
export class FakeXhr {
  static instances: FakeXhr[] = []
  method = ''
  url = ''
  headers: Record<string, string> = {}
  body: unknown = null
  withCredentials = false
  status = 0
  responseText = ''
  upload: {
    onprogress: ((e: { lengthComputable: boolean; loaded: number; total: number }) => void) | null
  } = {
    onprogress: null,
  }
  onload: (() => void) | null = null
  onerror: (() => void) | null = null
  onabort: (() => void) | null = null
  responseHeaders = ''
  constructor() {
    FakeXhr.instances.push(this)
  }
  open(method: string, url: string) {
    this.method = method
    this.url = url
  }
  setRequestHeader(name: string, value: string) {
    this.headers[name] = value
  }
  getAllResponseHeaders() {
    return this.responseHeaders
  }
  send(body: unknown) {
    this.body = body
  }
  abort() {
    this.onabort?.()
  }
  finish(status: number, body: unknown, headers = '') {
    this.status = status
    this.responseText = typeof body === 'string' ? body : JSON.stringify(body)
    this.responseHeaders = headers
    this.onload?.()
  }
}

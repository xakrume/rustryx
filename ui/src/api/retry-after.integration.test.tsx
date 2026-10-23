import { QueryClientProvider, useQuery } from '@tanstack/react-query'
import { render, screen, waitFor } from '@testing-library/react'
import { http, HttpResponse } from 'msw'
import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { Banners } from '@/features/shell/banners'
import { meBody } from '@/mocks/session'
import { server } from '@/test/server'
import { api } from './client'
import { createQueryClient } from './query-client'
import { unwrap } from './request'
import { installServiceStatus } from './service-status'

let uninstall: () => void = () => {}
beforeEach(() => {
  uninstall = installServiceStatus()
})
afterEach(() => uninstall())

function Probe() {
  const info = useQuery({
    queryKey: ['probe'],
    queryFn: ({ signal }) => unwrap(api.GET('/api/v1/system/info', { signal })),
  })
  return <p>{info.isSuccess ? 'loaded' : info.isError ? 'failed' : 'waiting'}</p>
}

describe('Retry-After, end to end through the real query client', () => {
  it('shows the countdown and retries a GET by itself once the wait is over', async () => {
    let calls = 0
    server.use(
      http.get('http://localhost:3000/api/v1/me', () => HttpResponse.json(meBody(null))),
      http.get('http://localhost:3000/api/v1/system/info', () => {
        calls += 1
        return calls === 1
          ? HttpResponse.json(
              { error: { code: 'overloaded', message: 'busy', request_id: 'r1' } },
              { status: 503, headers: { 'Retry-After': '1' } },
            )
          : HttpResponse.json({ name: 'rustryx' })
      }),
    )
    render(
      <QueryClientProvider client={createQueryClient()}>
        <Banners />
        <Probe />
      </QueryClientProvider>,
    )
    expect(await screen.findByText(/The server is busy\. Try again in \d+ s\./)).toBeInTheDocument()
    expect(screen.getByText('waiting')).toBeInTheDocument()
    await waitFor(() => expect(screen.getByText('loaded')).toBeInTheDocument(), { timeout: 4000 })
    expect(calls).toBe(2)
    await waitFor(() => expect(screen.queryByText(/The server is busy/)).not.toBeInTheDocument(), {
      timeout: 3000,
    })
  })

  it('must_not_retry_a_403_and_reports_it_at_once', async () => {
    let calls = 0
    server.use(
      http.get('http://localhost:3000/api/v1/me', () => HttpResponse.json(meBody(null))),
      http.get('http://localhost:3000/api/v1/system/info', () => {
        calls += 1
        return HttpResponse.json(
          { error: { code: 'forbidden', message: 'no', request_id: 'r' } },
          { status: 403 },
        )
      }),
    )
    render(
      <QueryClientProvider client={createQueryClient()}>
        <Probe />
      </QueryClientProvider>,
    )
    await waitFor(() => expect(screen.getByText('failed')).toBeInTheDocument())
    expect(calls).toBe(1)
  })
})

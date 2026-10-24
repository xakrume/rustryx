import { configure, screen, within } from '@testing-library/react'
import { http, HttpResponse } from 'msw'
import { describe, expect, it } from 'vitest'
import { expectNoA11yViolations, renderApp } from '@/test/utils'

// The whole suite runs in parallel workers: the first render of a page can be slow.
configure({ asyncUtilTimeout: 5000 })

const info = (over: Record<string, unknown> = {}) =>
  HttpResponse.json({
    name: 'rustryx',
    version: '0.1.0',
    license: 'MIT',
    build: { commit: '3ef1ba26', built_at: '2026-10-24T09:15:30Z', profile: 'release' },
    supply_chain: {
      sbom_url: 'https://example.org/rustryx.spdx.json',
      signature_hint: 'cosign verify ghcr.io/example/rustryx:0.1.0',
    },
    third_party_licenses_url: '/THIRD-PARTY-LICENSES.txt',
    banner: null,
    ...over,
  })

describe('about page', () => {
  it('shows version, build and how to verify the supply chain, to an anonymous visitor', async () => {
    const { container } = renderApp('/about')
    expect(
      await screen.findByRole('heading', { level: 1, name: 'About Rustryx' }),
    ).toBeInTheDocument()
    expect(await screen.findByText('0.1.0')).toBeInTheDocument()
    expect(screen.getByText('3ef1ba26')).toBeInTheDocument()
    expect(screen.getByText('cosign verify ghcr.io/example/rustryx:0.1.0')).toBeInTheDocument()
    expect(screen.getByRole('link', { name: /Read the third-party licenses/ })).toHaveAttribute(
      'href',
      '/THIRD-PARTY-LICENSES.txt',
    )
    expect(screen.getByText('This server does not publish an SBOM.')).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('links an SBOM that is published, in a new tab', async () => {
    renderApp('/about', {
      overrides: [http.get('/api/v1/system/info', () => info())],
    })
    const link = await screen.findByRole('link', { name: /Open the SBOM/ })
    expect(link).toHaveAttribute('href', 'https://example.org/rustryx.spdx.json')
    expect(link).toHaveAttribute('rel', 'noopener noreferrer')
    expect(link).toHaveAttribute('target', '_blank')
  })

  it('must_not_link_an_address_that_is_not_a_web_address', async () => {
    renderApp('/about', {
      overrides: [
        http.get('/api/v1/system/info', () =>
          info({
            supply_chain: { sbom_url: 'javascript:alert(1)', signature_hint: null },
            third_party_licenses_url: 'data:text/html,x',
          }),
        ),
      ],
    })
    expect(await screen.findByText('This server does not publish an SBOM.')).toBeInTheDocument()
    expect(screen.getByText('No license list is published.')).toBeInTheDocument()
    expect(screen.getByText('No signature verification hint is published.')).toBeInTheDocument()
    expect(screen.queryByRole('link', { name: /SBOM|licenses/ })).not.toBeInTheDocument()
  })

  it('shows the administrators message when there is one', async () => {
    renderApp('/about', {
      overrides: [http.get('/api/v1/system/info', () => info({ banner: 'Maintenance on Friday' }))],
    })
    const alert = await screen.findByText('Maintenance on Friday')
    expect(
      within(alert.closest('[role="status"]') as HTMLElement).getByText(
        'Message from the administrators',
      ),
    ).toBeInTheDocument()
  })

  it('shows the error with code and request id', async () => {
    renderApp('/about', {
      overrides: [
        http.get('/api/v1/system/info', () =>
          HttpResponse.json(
            { error: { code: 'internal_error', message: 'Boom', request_id: 'req-about' } },
            { status: 500, headers: { 'X-Request-Id': 'req-about' } },
          ),
        ),
      ],
    })
    expect(
      await screen.findByText('The server information could not be loaded'),
    ).toBeInTheDocument()
    expect(screen.getByText('req-about')).toBeInTheDocument()
  })
})

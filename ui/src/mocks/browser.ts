import { setupWorker } from 'msw/browser'
import { handlers } from './handlers'

/**
 * Starts the in-browser mock (MSW service worker). Imported dynamically from main.tsx only
 * when __API_MOCK__ is true, so the production bundle contains neither MSW nor the fixtures.
 *
 * Limit: a service worker cannot mock top level navigations, so the SSO button (which
 * navigates the page to /api/v1/auth/oidc/login) goes nowhere in mock mode. Use
 * /login?error=oidc_callback_invalid&request_id=abc to see the failed callback state.
 */
export async function startMockWorker(): Promise<void> {
  const worker = setupWorker(...handlers)
  await worker.start({
    onUnhandledRequest: 'bypass',
    quiet: true,
    serviceWorker: { url: '/mockServiceWorker.js' },
  })
}

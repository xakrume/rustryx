import { QueryClientProvider, type QueryClient } from '@tanstack/react-query'
import { createMemoryHistory, RouterProvider } from '@tanstack/react-router'
import { render, type RenderOptions, type RenderResult } from '@testing-library/react'
import { axe } from 'jest-axe'
import type { RequestHandler } from 'msw'
import type { ReactElement, ReactNode } from 'react'
import { expect } from 'vitest'
import { createQueryClient } from '@/api/query-client'
import { createAppRouter } from '@/app/router'
import { Toaster } from '@/components/toaster'
import { TooltipProvider } from '@/components/ui/tooltip'
import { ThemeProvider } from '@/features/theme/theme-provider'
import { handlers } from '@/mocks/handlers'
import { startSession } from '@/mocks/session'
import { server } from './server'

/** A query client that fails fast: tests assert on errors, they must not wait for retries. */
export function createTestQueryClient(): QueryClient {
  const client = createQueryClient()
  client.setDefaultOptions({
    queries: { ...client.getDefaultOptions().queries, retry: false, gcTime: Infinity },
    mutations: { retry: false },
  })
  return client
}

export function Providers({ children, client }: { children: ReactNode; client?: QueryClient }) {
  const queryClient = client ?? createTestQueryClient()
  return (
    <QueryClientProvider client={queryClient}>
      <ThemeProvider>
        <TooltipProvider delayDuration={0}>
          {children}
          <Toaster />
        </TooltipProvider>
      </ThemeProvider>
    </QueryClientProvider>
  )
}

/** Render a component with the app providers (query client, theme, tooltips, toasts). */
export function renderWithProviders(
  ui: ReactElement,
  options: Omit<RenderOptions, 'wrapper'> & { client?: QueryClient } = {},
): RenderResult & { client: QueryClient } {
  const client = options.client ?? createTestQueryClient()
  const result = render(ui, {
    ...options,
    wrapper: ({ children }) => <Providers client={client}>{children}</Providers>,
  })
  return { ...result, client }
}

export interface RenderAppOptions {
  /** Start with a mock session of this account. Omit for an anonymous visitor. */
  user?: 'admin' | 'alice'
  /**
   * Handlers that replace the mock for this test. They are registered AFTER the mock handlers
   * (`server.use` puts the latest first), so they win; registering them yourself before
   * calling renderApp would lose to the mock.
   */
  overrides?: RequestHandler[]
}

/**
 * Render the whole application (router, shell, pages) at `path`, against the SAME mock
 * handlers the browser mock uses, with an in memory history. Integration tests of navigation,
 * gating and the login flow use this.
 */
export function renderApp(path = '/', options: RenderAppOptions = {}) {
  server.use(...handlers)
  if (options.overrides) server.use(...options.overrides)
  if (options.user) startSession(options.user)
  const queryClient = createTestQueryClient()
  const history = createMemoryHistory({ initialEntries: [path] })
  const router = createAppRouter({ queryClient, history })
  const result = render(
    <QueryClientProvider client={queryClient}>
      <ThemeProvider>
        <TooltipProvider delayDuration={0}>
          <RouterProvider router={router} />
          <Toaster />
        </TooltipProvider>
      </ThemeProvider>
    </QueryClientProvider>,
  )
  return { ...result, router, queryClient, history }
}

/**
 * Fails when axe finds a violation in `container`. Two rules are off because jsdom cannot
 * evaluate them: color-contrast (no layout, no computed colors; the token script and the
 * Playwright axe scan cover it) and region (a fragment is not a whole page).
 */
export async function expectNoA11yViolations(container: Element): Promise<void> {
  const results = await axe(container, {
    rules: { 'color-contrast': { enabled: false }, region: { enabled: false } },
  })
  expect(results).toHaveNoViolations()
}

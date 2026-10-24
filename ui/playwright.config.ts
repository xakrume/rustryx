import { defineConfig, devices } from '@playwright/test'

// Page agents that work side by side pick their own port and build output through these two
// variables (E2E_PORT, E2E_SERVER_COMMAND); without them the defaults below are used.
const PORT = Number(process.env.E2E_PORT ?? 4173)

/**
 * The end to end tests run against the PRODUCTION style build in mock mode (`build:mock`)
 * served by `vite preview`, which sends the real Content-Security-Policy (scripts/csp.mjs).
 * So the smoke test exercises the same static files and the same CSP the Rust server will
 * ship, with the in-browser mock instead of a backend. A CSP violation fails the tests.
 */
export default defineConfig({
  testDir: 'e2e',
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  use: {
    baseURL: `http://localhost:${PORT}`,
    trace: 'retain-on-failure',
    // The mock answers within a few hundred milliseconds; animations are off for stable shots.
    reducedMotion: 'reduce',
  },
  projects: [{ name: 'chromium', use: { ...devices['Desktop Chrome'] } }],
  webServer: {
    command: process.env.E2E_SERVER_COMMAND ?? 'npm run build:mock && npm run preview:mock',
    url: `http://localhost:${PORT}`,
    reuseExistingServer: !process.env.CI,
    timeout: 240_000,
  },
})

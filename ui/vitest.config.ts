import { fileURLToPath } from 'node:url'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vitest/config'

// Separate from vite.config.ts on purpose: tests need neither the router plugin (the route
// tree is committed) nor Tailwind, and must never pick up the dev proxy or the mock switch.
export default defineConfig({
  plugins: [react()],
  resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
  define: {
    // Unit tests install their own MSW server (src/test/server.ts) with explicit handlers.
    __API_MOCK__: false,
    __DEMO__: true,
    __APP_VERSION__: JSON.stringify('0.0.0-test'),
  },
  test: {
    environment: './src/test/jsdom-environment.ts',
    setupFiles: ['./src/test/setup.ts'],
    include: ['src/**/*.test.{ts,tsx}', 'scripts/**/*.test.mjs'],
    css: false,
    // Absolute, because Node's Request cannot resolve a relative URL. jsdom serves http://localhost:3000.
    env: { VITE_API_BASE_URL: 'http://localhost:3000' },
    restoreMocks: true,
    coverage: {
      provider: 'v8',
      reporter: ['text-summary', 'lcov'],
      include: ['src/**/*.{ts,tsx}'],
      // Generated, fixtures for the mock, and the entry points that only wire things together.
      exclude: [
        'src/api/schema.d.ts',
        'src/routeTree.gen.ts',
        'src/mocks/generated/**',
        'src/test/**',
        'src/**/*.test.{ts,tsx}',
        'src/main.tsx',
        'src/mocks/browser.ts',
        'src/features/demo/**',
        'src/routes/**',
      ],
    },
    testTimeout: 15_000,
  },
})

import { readFileSync } from 'node:fs'
import type { IncomingMessage, ServerResponse } from 'node:http'
import { fileURLToPath, URL } from 'node:url'
import tailwindcss from '@tailwindcss/vite'
import { tanstackRouter } from '@tanstack/router-plugin/vite'
import react from '@vitejs/plugin-react'
import { defineConfig, loadEnv, type Plugin, type ProxyOptions } from 'vite'
import { securityHeaders } from './scripts/csp.mjs'

const root = fileURLToPath(new URL('.', import.meta.url))
const workerFile = fileURLToPath(
  new URL('./node_modules/msw/lib/mockServiceWorker.js', import.meta.url),
)
const pkg = JSON.parse(readFileSync(new URL('./package.json', import.meta.url), 'utf8')) as {
  version: string
}

/**
 * The MSW service worker script must be served from the site root so its scope covers
 * every route. It is generated from node_modules instead of being committed to public/:
 * a committed copy would ship in the production dist, where no mock may exist.
 */
function mswWorker(options: { serve: boolean; emit: boolean }): Plugin {
  const serveWorker = (req: IncomingMessage, res: ServerResponse, next: () => void) => {
    if ((req.url ?? '').split('?')[0] !== '/mockServiceWorker.js') return next()
    res.setHeader('Content-Type', 'text/javascript')
    res.setHeader('Service-Worker-Allowed', '/')
    res.end(readFileSync(workerFile))
  }
  return {
    name: 'rustryx-msw-worker',
    configureServer(server) {
      if (options.serve) server.middlewares.use(serveWorker)
    },
    generateBundle() {
      if (options.emit) {
        this.emitFile({
          type: 'asset',
          fileName: 'mockServiceWorker.js',
          source: readFileSync(workerFile),
        })
      }
    },
  }
}

/**
 * Dev proxy to a real backend. The browser talks to the dev server origin, so the
 * backend would see a foreign Origin and answer 403 csrf_rejected on every mutation
 * (docs/api-conventions.md 11.3). Rewriting Origin to the backend's own origin makes the
 * proxied request look same origin, which is exactly what production is. This is a
 * development convenience only: nothing of it exists in the production build.
 */
function proxyTo(target: string): ProxyOptions {
  return {
    target,
    changeOrigin: true,
    secure: process.env.RUSTRYX_API_INSECURE !== '1',
    configure: (proxy) => {
      proxy.on('proxyReq', (proxyReq) => proxyReq.setHeader('origin', new URL(target).origin))
    },
  }
}

export default defineConfig(({ mode, command }) => {
  const env = loadEnv(mode, root, '')
  const backend = (env.RUSTRYX_API_URL ?? '').trim()
  // The mock is on in `mock` mode, and in plain dev when no backend is configured.
  // `production` never mocks: a stray RUSTRYX_API_URL or VITE_* value cannot switch it on.
  const useMock = mode === 'mock' || (mode === 'development' && backend === '')
  const demo = mode !== 'production'

  return {
    plugins: [
      tanstackRouter({
        target: 'react',
        autoCodeSplitting: true,
        routesDirectory: 'src/routes',
        generatedRouteTree: 'src/routeTree.gen.ts',
        quoteStyle: 'single',
        semicolons: false,
      }),
      react(),
      tailwindcss(),
      mswWorker({ serve: useMock && command === 'serve', emit: useMock && command === 'build' }),
    ],
    resolve: { alias: { '@': fileURLToPath(new URL('./src', import.meta.url)) } },
    define: {
      __API_MOCK__: JSON.stringify(useMock),
      __DEMO__: JSON.stringify(demo),
      __APP_VERSION__: JSON.stringify(pkg.version),
    },
    server: {
      port: 5173,
      proxy:
        backend && !useMock
          ? { '/api': proxyTo(backend), '/repository': proxyTo(backend) }
          : undefined,
    },
    preview: {
      // The same policy the Rust server sends, so the smoke test fails on a CSP violation.
      headers: securityHeaders,
    },
    build: {
      target: 'es2022',
      // Zero: no asset becomes a data: URI, so the policy needs no `data:` source anywhere.
      assetsInlineLimit: 0,
      sourcemap: false,
      // The preload polyfill is only for browsers older than the es2022 target.
      modulePreload: { polyfill: false },
      cssCodeSplit: true,
      rollupOptions: {
        output: {
          // Only libraries every page needs are grouped. They change rarely, so splitting them
          // keeps them cached across deploys while the application chunk changes. Radix and the
          // rest stay with the routes that use them: one shared chunk would drag every Radix
          // package into the initial load.
          manualChunks(id: string) {
            if (/node_modules\/(react|react-dom|scheduler)\//.test(id)) return 'vendor-react'
            if (
              /node_modules\/@tanstack\/(react-router|router-core|history|store|react-store|react-query|query-core)\//.test(
                id,
              )
            ) {
              return 'vendor-tanstack'
            }
            return undefined
          },
        },
      },
    },
  }
})

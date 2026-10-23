import { createRoot } from 'react-dom/client'
import { installServiceStatus } from '@/api/service-status'
import { App } from '@/app/app'
import '@/styles/index.css'

/**
 * Entry point. `__API_MOCK__` is a build time constant (vite.config.ts): when it is false the
 * bundler removes this dynamic import, so the production bundle has no MSW and no fixtures.
 */
async function enableMocking(): Promise<void> {
  if (!__API_MOCK__) return
  const { startMockWorker } = await import('@/mocks/browser')
  await startMockWorker()
}

// Before the first request, so the very first failure already reaches the banners.
installServiceStatus()

const container = document.getElementById('root')
if (container === null) throw new Error('index.html has no #root element')

void enableMocking().then(() => {
  createRoot(container).render(<App />)
})

import { createFileRoute, notFound } from '@tanstack/react-router'
import { lazy } from 'react'

const text = (value: unknown): string | undefined => (typeof value === 'string' ? value : undefined)

// `__DEMO__` is false in the production build, so the bundler drops the dynamic import and the
// gallery never ships; the route stays in the tree but answers 404.
const ComponentsPage = __DEMO__ ? lazy(() => import('@/features/demo/components-page')) : null

export const Route = createFileRoute('/_app/__components')({
  // Hand written, not zod: validateSearch is part of the entry bundle (see routes/login.tsx).
  validateSearch: (search: Record<string, unknown>) => ({
    q: text(search.q),
    format: text(search.format),
    repo_type: text(search.repo_type),
    sort: text(search.sort),
  }),
  beforeLoad: () => {
    if (!__DEMO__) throw notFound()
  },
  component: () => (ComponentsPage ? <ComponentsPage /> : null),
})

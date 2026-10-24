import { createFileRoute } from '@tanstack/react-router'
import { RepositoryComponentsPage } from '@/features/components/components-table-page'
import { text } from '@/features/repositories/search-params'

export const Route = createFileRoute('/_app/repositories/$name/components')({
  validateSearch: (
    search: Record<string, unknown>,
  ): { q?: string; group?: string; version?: string; sort?: string } => ({
    q: text(search.q),
    group: text(search.group),
    version: text(search.version),
    sort: text(search.sort),
  }),
  staticData: { crumb: 'pages.components' },
  component: RepositoryComponentsPage,
})

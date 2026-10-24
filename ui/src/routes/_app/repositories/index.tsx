import { createFileRoute } from '@tanstack/react-router'
import { RepositoriesPage } from '@/features/repositories/repositories-page'
import { text } from '@/features/repositories/search-params'

export const Route = createFileRoute('/_app/repositories/')({
  validateSearch: (
    search: Record<string, unknown>,
  ): { q?: string; format?: string; repo_type?: string; online?: string; sort?: string } => ({
    q: text(search.q),
    format: text(search.format),
    repo_type: text(search.repo_type),
    online: text(search.online),
    sort: text(search.sort),
  }),
  component: RepositoriesPage,
})

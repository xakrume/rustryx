import { createFileRoute } from '@tanstack/react-router'
import { BrowsePage } from '@/features/browse/browse-page'
import { text } from '@/features/repositories/search-params'

export const Route = createFileRoute('/_app/repositories/$name/browse')({
  validateSearch: (search: Record<string, unknown>): { path?: string; q?: string } => ({
    path: text(search.path),
    q: text(search.q),
  }),
  staticData: { crumb: 'pages.browse' },
  component: BrowsePage,
})

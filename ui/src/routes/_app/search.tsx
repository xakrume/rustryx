import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/search')({
  validateSearch: (search: Record<string, unknown>): { q?: string } => ({
    q: typeof search.q === 'string' ? search.q : undefined,
  }),
  staticData: { crumb: 'nav.items.search' },
  component: () => <PlaceholderPage titleKey="nav.items.search" route="/search" milestone="M1" />,
})

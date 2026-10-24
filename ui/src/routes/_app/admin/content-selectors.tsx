import { createFileRoute } from '@tanstack/react-router'
import { pickText } from '@/features/roles/search-params'
import { SelectorsPage } from '@/features/selectors/selectors-page'

export const Route = createFileRoute('/_app/admin/content-selectors')({
  staticData: { crumb: 'nav.items.contentSelectors', access: { area: 'selectors' } },
  validateSearch: (search: Record<string, unknown>) =>
    pickText(search, ['q', 'edit', 'new'] as const),
  component: SelectorsPage,
})

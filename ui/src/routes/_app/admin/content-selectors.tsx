import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/admin/content-selectors')({
  staticData: { crumb: 'nav.items.contentSelectors', access: { area: 'selectors' } },
  component: () => (
    <PlaceholderPage
      titleKey="nav.items.contentSelectors"
      route="/admin/content-selectors"
      milestone="M1"
    />
  ),
})

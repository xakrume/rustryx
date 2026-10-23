import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/components/$id')({
  staticData: { crumb: 'pages.componentDetail' },
  component: () => (
    <PlaceholderPage titleKey="pages.componentDetail" route="/components/:id" milestone="M1" />
  ),
})

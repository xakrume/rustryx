import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/assets/$id')({
  staticData: { crumb: 'pages.assetDetail' },
  component: () => (
    <PlaceholderPage titleKey="pages.assetDetail" route="/assets/:id" milestone="M1" />
  ),
})

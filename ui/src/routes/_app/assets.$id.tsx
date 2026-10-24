import { createFileRoute } from '@tanstack/react-router'
import { AssetDetailPage } from '@/features/components/asset-detail-page'

export const Route = createFileRoute('/_app/assets/$id')({
  staticData: { crumb: 'pages.assetDetail' },
  component: AssetDetailPage,
})

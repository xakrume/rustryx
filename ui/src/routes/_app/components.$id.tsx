import { createFileRoute } from '@tanstack/react-router'
import { ComponentDetailPage } from '@/features/components/component-detail-page'

export const Route = createFileRoute('/_app/components/$id')({
  staticData: { crumb: 'pages.componentDetail' },
  component: ComponentDetailPage,
})

import { createFileRoute } from '@tanstack/react-router'
import { StatusPage } from '@/features/system/status-page'

export const Route = createFileRoute('/_app/admin/system/status')({
  staticData: { crumb: 'nav.items.status', access: { area: 'system' } },
  component: StatusPage,
})

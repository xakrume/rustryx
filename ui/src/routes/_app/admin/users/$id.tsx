import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/admin/users/$id')({
  staticData: { crumb: 'pages.userDetail' },
  component: () => (
    <PlaceholderPage titleKey="pages.userDetail" route="/admin/users/:id" milestone="M1" />
  ),
})

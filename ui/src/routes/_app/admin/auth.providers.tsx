import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/admin/auth/providers')({
  staticData: { crumb: 'nav.items.authProviders', access: 'admin' },
  component: () => (
    <PlaceholderPage
      titleKey="nav.items.authProviders"
      route="/admin/auth/providers"
      milestone="M1"
    />
  ),
})

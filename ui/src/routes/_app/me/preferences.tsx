import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/me/preferences')({
  staticData: { crumb: 'nav.items.preferences' },
  component: () => (
    <PlaceholderPage titleKey="nav.items.preferences" route="/me/preferences" milestone="M1" />
  ),
})

import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/me/')({
  component: () => <PlaceholderPage titleKey="nav.items.profile" route="/me" milestone="M1" />,
})

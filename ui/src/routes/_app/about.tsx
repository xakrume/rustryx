import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/about')({
  staticData: { crumb: 'nav.items.about' },
  component: () => <PlaceholderPage titleKey="nav.items.about" route="/about" milestone="M1" />,
})

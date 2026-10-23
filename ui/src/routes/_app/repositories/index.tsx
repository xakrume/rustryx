import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/repositories/')({
  component: () => (
    <PlaceholderPage titleKey="nav.items.repositories" route="/repositories" milestone="M1" />
  ),
})

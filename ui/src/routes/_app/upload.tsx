import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/upload')({
  staticData: { crumb: 'nav.items.upload', access: { can: 'upload' } },
  component: () => <PlaceholderPage titleKey="nav.items.upload" route="/upload" milestone="M1" />,
})

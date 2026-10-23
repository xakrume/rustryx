import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/admin/blob-stores/')({
  component: () => (
    <PlaceholderPage titleKey="nav.items.blobStores" route="/admin/blob-stores" milestone="M1" />
  ),
})

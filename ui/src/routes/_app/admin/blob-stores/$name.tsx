import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/admin/blob-stores/$name')({
  staticData: { crumb: ({ params }) => params.name ?? '' },
  component: () => (
    <PlaceholderPage
      titleKey="pages.blobStoreDetail"
      route="/admin/blob-stores/:name"
      milestone="M1"
    />
  ),
})

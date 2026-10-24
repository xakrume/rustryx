import { createFileRoute } from '@tanstack/react-router'
import { BlobStoreDetailPage } from '@/features/blob-stores/blob-store-detail-page'

export const Route = createFileRoute('/_app/admin/blob-stores/$name')({
  staticData: { crumb: ({ params }) => params.name ?? '' },
  component: function BlobStoreRoute() {
    const { name } = Route.useParams()
    return <BlobStoreDetailPage name={name} />
  },
})

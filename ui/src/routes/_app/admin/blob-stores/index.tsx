import { createFileRoute } from '@tanstack/react-router'
import { BlobStoresPage } from '@/features/blob-stores/blob-stores-page'
import { text } from '@/features/system/search-params'

// Optional keys keep every plain link to this page valid: nothing is required in the address.
interface BlobStoresSearch {
  q?: string
  kind?: string
  view?: string
}

// Filters and the create view live in the address bar: `?q=&kind=&view=create`.
export const Route = createFileRoute('/_app/admin/blob-stores/')({
  validateSearch: (search: Record<string, unknown>): BlobStoresSearch => ({
    q: text(search.q),
    kind: text(search.kind),
    view: text(search.view),
  }),
  component: BlobStoresPage,
})

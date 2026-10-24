import { createFileRoute } from '@tanstack/react-router'
import { ImagePage } from '@/features/repositories/image-page'
import { text } from '@/features/repositories/search-params'

export const Route = createFileRoute('/_app/repositories/$name/images/$image')({
  validateSearch: (search: Record<string, unknown>): { q?: string; sort?: string } => ({
    q: text(search.q),
    sort: text(search.sort),
  }),
  staticData: { crumb: ({ params }) => params.image ?? '' },
  component: ImagePage,
})

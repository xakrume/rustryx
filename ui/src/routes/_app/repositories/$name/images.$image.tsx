import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/repositories/$name/images/$image')({
  staticData: { crumb: ({ params }) => params.image ?? '' },
  component: () => (
    <PlaceholderPage
      titleKey="pages.image"
      route="/repositories/:name/images/:image"
      milestone="M1"
    />
  ),
})

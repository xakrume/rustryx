import { createFileRoute } from '@tanstack/react-router'
import { AboutPage } from '@/features/system/about-page'

// Public: anonymous visitors may read it (docs/ui-page-inventory.md, 8.7).
export const Route = createFileRoute('/_app/about')({
  staticData: { crumb: 'nav.items.about' },
  component: AboutPage,
})

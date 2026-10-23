import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/repositories/new')({
  staticData: { crumb: 'pages.createRepository', access: { can: 'create_repository' } },
  component: () => (
    <PlaceholderPage titleKey="pages.createRepository" route="/repositories/new" milestone="M1" />
  ),
})

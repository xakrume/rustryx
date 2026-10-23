import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/repositories/$name/settings')({
  staticData: { crumb: 'pages.repositorySettings', access: 'admin' },
  component: () => (
    <PlaceholderPage
      titleKey="pages.repositorySettings"
      route="/repositories/:name/settings"
      milestone="M1"
    />
  ),
})

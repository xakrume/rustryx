import { createFileRoute } from '@tanstack/react-router'
import { PlaceholderPage } from '@/features/placeholder/placeholder-page'

export const Route = createFileRoute('/_app/repositories/$name/activity')({
  staticData: { crumb: 'pages.activity', access: { area: 'audit' } },
  component: () => (
    <PlaceholderPage
      titleKey="pages.activity"
      route="/repositories/:name/activity"
      milestone="M1"
    />
  ),
})

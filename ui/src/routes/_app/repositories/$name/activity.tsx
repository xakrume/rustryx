import { createFileRoute } from '@tanstack/react-router'
import { RepositoryActivityPage } from '@/features/repositories/activity-page'

export const Route = createFileRoute('/_app/repositories/$name/activity')({
  staticData: { crumb: 'pages.activity', access: { area: 'audit' } },
  component: RepositoryActivityPage,
})

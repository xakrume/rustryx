import { createFileRoute } from '@tanstack/react-router'
import { RepositorySettingsPage } from '@/features/repositories/settings-page'

export const Route = createFileRoute('/_app/repositories/$name/settings')({
  staticData: { crumb: 'pages.repositorySettings', access: 'admin' },
  component: RepositorySettingsPage,
})

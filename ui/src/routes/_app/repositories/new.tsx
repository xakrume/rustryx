import { createFileRoute } from '@tanstack/react-router'
import { CreateRepositoryPage } from '@/features/repositories/create-wizard'

export const Route = createFileRoute('/_app/repositories/new')({
  staticData: { crumb: 'pages.createRepository', access: { can: 'create_repository' } },
  component: CreateRepositoryPage,
})

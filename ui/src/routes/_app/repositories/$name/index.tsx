import { createFileRoute } from '@tanstack/react-router'
import { RepositoryOverviewPage } from '@/features/repositories/overview-page'

export const Route = createFileRoute('/_app/repositories/$name/')({
  component: RepositoryOverviewPage,
})

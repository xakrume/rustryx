import { createFileRoute } from '@tanstack/react-router'
import { RepositoryLayout } from '@/features/repositories/repository-layout'

// Layout route: the repository's heading, facts and tabs; the tab pages render below it.
export const Route = createFileRoute('/_app/repositories/$name')({
  staticData: { crumb: ({ params }) => params.name ?? '' },
  component: RepositoryLayout,
})

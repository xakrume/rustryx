import { createFileRoute } from '@tanstack/react-router'
import { pickText } from '@/features/roles/search-params'
import { UsersPage } from '@/features/users/users-page'

export const Route = createFileRoute('/_app/admin/users/')({
  validateSearch: (search: Record<string, unknown>) =>
    pickText(search, ['q', 'source', 'enabled', 'role', 'sort', 'new'] as const),
  component: UsersPage,
})

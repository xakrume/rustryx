import { createFileRoute } from '@tanstack/react-router'
import { AnonymousPage } from '@/features/anonymous/anonymous-page'
import { pickText } from '@/features/roles/search-params'

export const Route = createFileRoute('/_app/admin/anonymous')({
  staticData: { crumb: 'nav.items.anonymousAccess', access: 'admin' },
  validateSearch: (search: Record<string, unknown>) =>
    pickText(search, ['q', 'format', 'allow_anonymous'] as const),
  component: AnonymousPage,
})

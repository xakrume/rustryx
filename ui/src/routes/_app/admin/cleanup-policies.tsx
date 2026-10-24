import { createFileRoute } from '@tanstack/react-router'
import { CleanupPage } from '@/features/cleanup/cleanup-page'
import { text } from '@/features/system/search-params'

interface CleanupSearch {
  q?: string
  format?: string
  /** A policy id, or `new`: the editor replaces the list. */
  edit?: string
}

export const Route = createFileRoute('/_app/admin/cleanup-policies')({
  staticData: { crumb: 'nav.items.cleanupPolicies', access: 'admin' },
  validateSearch: (search: Record<string, unknown>): CleanupSearch => ({
    q: text(search.q),
    format: text(search.format),
    edit: text(search.edit),
  }),
  component: CleanupPage,
})

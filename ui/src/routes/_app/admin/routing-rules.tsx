import { createFileRoute } from '@tanstack/react-router'
import { RoutingPage } from '@/features/routing/routing-page'
import { text } from '@/features/system/search-params'

interface RoutingSearch {
  q?: string
  /** A rule id, or `new`: the editor replaces the list. */
  edit?: string
}

export const Route = createFileRoute('/_app/admin/routing-rules')({
  staticData: { crumb: 'nav.items.routingRules', access: 'admin' },
  validateSearch: (search: Record<string, unknown>): RoutingSearch => ({
    q: text(search.q),
    edit: text(search.edit),
  }),
  component: RoutingPage,
})

import { createFileRoute } from '@tanstack/react-router'
import { text } from '@/features/repositories/search-params'
import { SearchPage } from '@/features/search/search-page'

interface SearchParams {
  kind?: string
  q?: string
  format?: string
  repository?: string
  group?: string
  name?: string
  version?: string
  checksum?: string
  path?: string
  sort?: string
}

export const Route = createFileRoute('/_app/search')({
  // `repo` is the short form the page inventory uses; the API calls the same filter `repository`.
  validateSearch: (search: Record<string, unknown>): SearchParams => ({
    kind: text(search.kind),
    q: text(search.q),
    format: text(search.format),
    repository: text(search.repository) ?? text(search.repo),
    group: text(search.group),
    name: text(search.name),
    version: text(search.version),
    checksum: text(search.checksum) ?? text(search.sha256),
    path: text(search.path),
    sort: text(search.sort),
  }),
  staticData: { crumb: 'nav.items.search' },
  component: SearchPage,
})

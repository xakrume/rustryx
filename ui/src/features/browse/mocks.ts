import { delay, http, type HttpHandler } from 'msw'
import type { components } from '@/api/schema'
import { visibleComponents } from '@/features/components/mock-content'
import { formatHandlers } from '@/features/repositories/mocks'
import { paginate } from '@/features/repositories/mock-page'
import { findVisible } from '@/features/repositories/mock-state'
import { problem } from '@/mocks/problem'
import { json, LATENCY_MS } from '@/mocks/store'

type TreeNode = components['schemas']['TreeNode']

interface Leaf {
  path: string
  assetId: string
  componentId: string
  size: number
  type: string
  updatedAt: string
  source: string | null
}

/**
 * `GET /api/v1/repositories/{name}/tree`: one folder level of the repository's content, folders
 * first and then files, each sorted by name, with the serving member named for a group. OCI
 * repositories have no path tree (409 `unsupported_for_format`), as the contract says.
 */
export const handlers: HttpHandler[] = [
  // Registered here, ahead of the cleanup feature's own `/formats`: see `formatHandlers`.
  ...formatHandlers,
  http.get('/api/v1/repositories/:name/tree', async ({ request, params }) => {
    await delay(LATENCY_MS)
    const repo = findVisible(String(params.name))
    if (repo === undefined) return problem(404, 'repository_not_found', 'Repository not found')
    if (repo.format === 'docker' || repo.format === 'oci') {
      return problem(409, 'unsupported_for_format', 'This format has no path tree')
    }
    const url = new URL(request.url)
    const folder = url.searchParams.get('path') ?? '/'
    if (!folder.startsWith('/') || !folder.endsWith('/')) {
      return problem(400, 'invalid_query', 'path must start and end with a slash')
    }
    const q = url.searchParams.get('q')?.toLowerCase()

    const leaves: Leaf[] = visibleComponents(repo).flatMap((c) =>
      c.assets.map((a) => ({
        path: `/${a.path}`,
        assetId: a.id,
        componentId: c.id,
        size: a.size,
        type: a.contentType,
        updatedAt: a.createdAt,
        source: repo.repo_type === 'group' ? c.repository : null,
      })),
    )

    const folders = new Map<
      string,
      { children: Set<string>; source: string | null; updatedAt: string }
    >()
    const files: TreeNode[] = []
    for (const leaf of leaves) {
      if (!leaf.path.startsWith(folder)) continue
      const rest = leaf.path.slice(folder.length)
      const slash = rest.indexOf('/')
      if (slash < 0) {
        files.push({
          name: rest,
          path: leaf.path,
          kind: 'asset',
          asset_id: leaf.assetId,
          component_id: leaf.componentId,
          size_bytes: leaf.size,
          content_type: leaf.type,
          updated_at: leaf.updatedAt,
          child_count: null,
          source_repository: leaf.source,
        })
        continue
      }
      const name = rest.slice(0, slash)
      const below = rest.slice(slash + 1)
      const entry = folders.get(name) ?? {
        children: new Set<string>(),
        source: leaf.source,
        updatedAt: leaf.updatedAt,
      }
      entry.children.add(below.split('/')[0] ?? '')
      folders.set(name, entry)
    }
    const folderNodes: TreeNode[] = [...folders.entries()].map(([name, entry]) => ({
      name,
      path: `${folder}${name}/`,
      kind: 'folder',
      asset_id: null,
      component_id: null,
      size_bytes: null,
      content_type: null,
      updated_at: entry.updatedAt,
      child_count: entry.children.size,
      source_repository: entry.source,
    }))
    const byName = (a: TreeNode, b: TreeNode): number =>
      a.name < b.name ? -1 : a.name > b.name ? 1 : 0
    const rows = [...folderNodes.sort(byName), ...files.sort(byName)].filter(
      (n) => q === undefined || n.name.toLowerCase().startsWith(q),
    )
    const result = paginate(rows, url, { limit: 200, max: 500 })
    return result instanceof Response ? result : json({ path: folder, ...result })
  }),
]

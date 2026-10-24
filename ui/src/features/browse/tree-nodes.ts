import type { TreeNodeData } from '@/components/tree/tree-model'
import { t } from '@/i18n'
import type { TreeEntry, TreePage } from './api'

export const ROOT_ID = '/'

export type BrowseNodeData =
  | { kind: 'folder'; entry: TreeEntry }
  | { kind: 'asset'; entry: TreeEntry }
  | { kind: 'more'; folder: string; after: string; loaded: number }
  | { kind: 'root'; repository: string }

export const dataOf = (node: TreeNodeData): BrowseNodeData | undefined =>
  node.data as BrowseNodeData | undefined

const MORE = '\u0000more:'

/** Rows for one page of a folder, with a trailing "load more" row when the server has another page. */
export function nodesOf(page: TreePage, folder: string, already = 0): TreeNodeData[] {
  const rows: TreeNodeData[] = page.items.map((entry) => ({
    id: entry.path,
    label: entry.name,
    hasChildren: entry.kind === 'folder',
    data: { kind: entry.kind, entry } satisfies BrowseNodeData,
  }))
  if (page.next !== null) {
    const loaded = already + page.items.length
    rows.push({
      id: `${folder}${MORE}${page.next}`,
      label: t('browse.loadMore', { count: loaded }),
      hasChildren: false,
      more: true,
      data: { kind: 'more', folder, after: page.next, loaded } satisfies BrowseNodeData,
    })
  }
  return rows
}

export function rootNode(repository: string): TreeNodeData {
  return {
    id: ROOT_ID,
    label: repository,
    hasChildren: true,
    data: { kind: 'root', repository } satisfies BrowseNodeData,
  }
}

/** `/a/b/c.jar` reveals `/`, `/a/`, `/a/b/`; a folder path `/a/b/` also opens itself. */
export function revealIdsFor(path: string | undefined): string[] {
  if (path === undefined || path === ROOT_ID) return [ROOT_ID]
  const parts = path.split('/').filter(Boolean)
  const isFolder = path.endsWith('/')
  const folders = isFolder ? parts : parts.slice(0, -1)
  return [ROOT_ID, ...folders.map((_, i) => `/${folders.slice(0, i + 1).join('/')}/`)]
}

/** A path from the address bar, normalised to a leading slash; anything that is not a path is "no selection". */
export function cleanPath(value: string | undefined): string | undefined {
  if (value === undefined || value === '') return undefined
  return value.startsWith('/') ? value : `/${value}`
}

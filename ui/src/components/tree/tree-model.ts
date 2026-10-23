/**
 * Pure model of the lazy tree: which nodes are visible, in which order, at which level. Kept
 * apart from the component so keyboard navigation and type-ahead are plain functions that
 * are easy to test, and the component only renders what this returns.
 */
export interface TreeNodeData {
  id: string
  label: string
  /** True for a folder: it can be expanded and its children are fetched on first expand. */
  hasChildren: boolean
  /** Free slot for the page: kind, size, a Lucide icon name. The tree never reads it. */
  data?: unknown
}

export type ChildState =
  { status: 'loading' } | { status: 'error' } | { status: 'loaded'; items: readonly TreeNodeData[] }

export interface VisibleRow {
  node: TreeNodeData
  /** 1 for roots, as aria-level counts. */
  level: number
  /** Position among siblings, 1 based, and the sibling count: aria-posinset and aria-setsize. */
  posInSet: number
  setSize: number
  parentId: string | null
  expanded: boolean
  child: ChildState | undefined
}

export function visibleRows(
  roots: readonly TreeNodeData[],
  expanded: ReadonlySet<string>,
  children: ReadonlyMap<string, ChildState>,
): VisibleRow[] {
  const rows: VisibleRow[] = []
  const walk = (nodes: readonly TreeNodeData[], level: number, parentId: string | null): void => {
    nodes.forEach((node, index) => {
      const isExpanded = node.hasChildren && expanded.has(node.id)
      const child = children.get(node.id)
      rows.push({
        node,
        level,
        posInSet: index + 1,
        setSize: nodes.length,
        parentId,
        expanded: isExpanded,
        child,
      })
      if (isExpanded && child?.status === 'loaded') walk(child.items, level + 1, node.id)
    })
  }
  walk(roots, 1, null)
  return rows
}

/**
 * Type-ahead: the next visible row after `fromIndex` whose label starts with `buffer`
 * (case insensitive), wrapping around. Typing the same letter repeatedly cycles through
 * the rows that start with it, as file managers do.
 */
export function findTypeAhead(
  rows: readonly VisibleRow[],
  fromIndex: number,
  buffer: string,
): number {
  if (rows.length === 0 || buffer === '') return -1
  const needle = buffer.toLowerCase()
  const cycling = [...needle].every((ch) => ch === needle[0])
  const prefix = cycling ? (needle[0] ?? needle) : needle
  // A multi character buffer may match the current row itself; a single letter moves on.
  const start = needle.length > 1 && !cycling ? fromIndex : fromIndex + 1
  for (let step = 0; step < rows.length; step += 1) {
    const index = (start + step) % rows.length
    if (rows[index]?.node.label.toLowerCase().startsWith(prefix)) return index
  }
  return -1
}

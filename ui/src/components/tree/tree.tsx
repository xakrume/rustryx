import {
  ChevronRight,
  Ellipsis,
  File,
  Folder,
  FolderOpen,
  Loader2,
  TriangleAlert,
} from 'lucide-react'
import {
  useCallback,
  useEffect,
  useMemo,
  useReducer,
  useRef,
  type KeyboardEvent,
  type ReactNode,
} from 'react'
import { t } from '@/i18n'
import { cn } from '@/lib/utils'
import {
  findTypeAhead,
  visibleRows,
  type ChildState,
  type TreeNodeData,
  type VisibleRow,
} from './tree-model'

export type { TreeNodeData } from './tree-model'

export interface TreeProps {
  /** Accessible name of the tree: "Repository content". */
  label: string
  roots: readonly TreeNodeData[]
  /** Fetch the children of a folder. Called once per expand; a failure is retried by expanding again. */
  loadChildren: (node: TreeNodeData, signal: AbortSignal) => Promise<readonly TreeNodeData[]>
  /**
   * Fetch the next page of a folder. Called with the folder's `more` row; the nodes it returns
   * take the place of that row. Needed only when `loadChildren` can return a `more` row.
   */
  loadMore?: (node: TreeNodeData, signal: AbortSignal) => Promise<readonly TreeNodeData[]>
  /**
   * Folder ids from the top down that are expanded once, as soon as each is visible: this reveals
   * a deep selection that came from the address bar. A user who collapses one is not overruled.
   */
  revealIds?: readonly string[]
  selectedId?: string | null
  /** Called on click, Enter and Space. Folders also toggle. */
  onSelect?: (node: TreeNodeData) => void
  /** Custom row content. The default is an icon and the label. */
  renderLabel?: (node: TreeNodeData, row: { expanded: boolean }) => ReactNode
  className?: string
}

interface State {
  expanded: ReadonlySet<string>
  children: ReadonlyMap<string, ChildState>
  focusedId: string | null
  /** Progress of the `more` rows, by row id. */
  more: ReadonlyMap<string, 'loading' | 'error'>
}

type Action =
  | { type: 'expand'; id: string }
  | { type: 'collapse'; id: string }
  | { type: 'child'; id: string; state: ChildState }
  | { type: 'focus'; id: string }
  | { type: 'more-status'; id: string; status: 'loading' | 'error' | null }
  | { type: 'append'; folderId: string; moreId: string; items: readonly TreeNodeData[] }

/** Every transition builds new Set and Map instances: React state is never mutated. */
function reducer(state: State, action: Action): State {
  switch (action.type) {
    case 'expand':
      return { ...state, expanded: new Set(state.expanded).add(action.id) }
    case 'collapse': {
      const expanded = new Set(state.expanded)
      expanded.delete(action.id)
      return { ...state, expanded }
    }
    case 'child':
      return { ...state, children: new Map(state.children).set(action.id, action.state) }
    case 'focus':
      return state.focusedId === action.id ? state : { ...state, focusedId: action.id }
    case 'more-status': {
      const more = new Map(state.more)
      if (action.status === null) more.delete(action.id)
      else more.set(action.id, action.status)
      return { ...state, more }
    }
    case 'append': {
      const current = state.children.get(action.folderId)
      if (current?.status !== 'loaded') return state
      // The `more` row is replaced by the next page, which may end with a new `more` row.
      const items = current.items.flatMap((node) =>
        node.id === action.moreId ? action.items : [node],
      )
      const more = new Map(state.more)
      more.delete(action.moreId)
      return {
        ...state,
        more,
        children: new Map(state.children).set(action.folderId, { status: 'loaded', items }),
      }
    }
  }
}

const TYPE_AHEAD_RESET_MS = 600

/**
 * ARIA tree (WAI-ARIA authoring practices, tree view): one tab stop, arrow keys, Home and End,
 * Right and Left to expand and collapse, type-ahead, and `aria-expanded`, `aria-level`,
 * `aria-posinset`, `aria-setsize`, `aria-selected` on every item. The rows are a flat list
 * with explicit levels, which keeps the DOM simple and virtualizable later.
 *
 * Children load lazily on first expand. While a folder loads its row has aria-busy and a
 * spinner, and a polite live region announces it; a failed load shows a warning icon on the
 * row and expanding again retries. No interactive element sits inside a row, so the tree
 * stays one focusable widget.
 */
export function Tree({
  label,
  roots,
  loadChildren,
  loadMore,
  revealIds,
  selectedId = null,
  onSelect,
  renderLabel,
  className,
}: TreeProps) {
  const [state, dispatch] = useReducer(reducer, {
    expanded: new Set<string>(),
    children: new Map<string, ChildState>(),
    focusedId: null,
    more: new Map<string, 'loading' | 'error'>(),
  })
  const rows = useMemo(
    () => visibleRows(roots, state.expanded, state.children),
    [roots, state.expanded, state.children],
  )
  const stateRef = useRef(state)
  stateRef.current = state
  const itemRefs = useRef(new Map<string, HTMLLIElement>())
  const revealed = useRef(new Set<string>())
  const aborters = useRef(new Map<string, AbortController>())
  const typeAhead = useRef({ buffer: '', timer: null as ReturnType<typeof setTimeout> | null })

  useEffect(
    () => () => {
      aborters.current.forEach((controller) => controller.abort())
      // An unmount abandons every running load. Forgetting the controllers lets the effect below
      // restart a load that React StrictMode (development) cancelled by remounting this one.
      aborters.current.clear()
      if (typeAhead.current.timer !== null) clearTimeout(typeAhead.current.timer)
    },
    [],
  )

  // The tab stop: the focused row, else the selected one, else the first.
  const tabStopId =
    rows.find((r) => r.node.id === state.focusedId)?.node.id ??
    rows.find((r) => r.node.id === selectedId)?.node.id ??
    rows[0]?.node.id

  const focusRow = useCallback((id: string) => {
    dispatch({ type: 'focus', id })
    itemRefs.current.get(id)?.focus()
  }, [])

  const load = useCallback(
    (node: TreeNodeData) => {
      aborters.current.get(node.id)?.abort()
      const controller = new AbortController()
      aborters.current.set(node.id, controller)
      dispatch({ type: 'child', id: node.id, state: { status: 'loading' } })
      loadChildren(node, controller.signal).then(
        (items) => {
          if (!controller.signal.aborted) {
            dispatch({ type: 'child', id: node.id, state: { status: 'loaded', items } })
          }
        },
        () => {
          if (!controller.signal.aborted) {
            dispatch({ type: 'child', id: node.id, state: { status: 'error' } })
          }
        },
      )
    },
    [loadChildren],
  )

  const expand = useCallback(
    (row: VisibleRow) => {
      if (!row.node.hasChildren) return
      dispatch({ type: 'expand', id: row.node.id })
      // Load on first expand, and again after a failure; a loaded folder keeps its children.
      if (row.child === undefined || row.child.status === 'error') load(row.node)
    },
    [load],
  )

  const loadMoreRow = useCallback(
    (row: VisibleRow) => {
      const folderId = row.parentId
      if (loadMore === undefined || folderId === null) return
      if (stateRef.current.more.get(row.node.id) === 'loading') return
      aborters.current.get(row.node.id)?.abort()
      const controller = new AbortController()
      aborters.current.set(row.node.id, controller)
      dispatch({ type: 'more-status', id: row.node.id, status: 'loading' })
      loadMore(row.node, controller.signal).then(
        (items) => {
          if (!controller.signal.aborted) {
            dispatch({ type: 'append', folderId, moreId: row.node.id, items })
          }
        },
        () => {
          if (!controller.signal.aborted) {
            dispatch({ type: 'more-status', id: row.node.id, status: 'error' })
          }
        },
      )
    },
    [loadMore],
  )

  const activate = useCallback(
    (row: VisibleRow) => {
      if (row.node.more) {
        loadMoreRow(row)
        return
      }
      onSelect?.(row.node)
      if (row.node.hasChildren) {
        if (row.expanded) dispatch({ type: 'collapse', id: row.node.id })
        else expand(row)
      }
    },
    [expand, loadMoreRow, onSelect],
  )

  function onKeyDown(event: KeyboardEvent<HTMLUListElement>): void {
    const index = rows.findIndex((r) => r.node.id === (state.focusedId ?? tabStopId))
    const row = rows[index]
    if (row === undefined) return
    const go = (target: number): void => {
      const next = rows[Math.max(0, Math.min(rows.length - 1, target))]
      if (next) focusRow(next.node.id)
    }
    switch (event.key) {
      case 'ArrowDown':
        event.preventDefault()
        go(index + 1)
        return
      case 'ArrowUp':
        event.preventDefault()
        go(index - 1)
        return
      case 'Home':
        event.preventDefault()
        go(0)
        return
      case 'End':
        event.preventDefault()
        go(rows.length - 1)
        return
      case 'ArrowRight':
        event.preventDefault()
        if (!row.node.hasChildren) return
        if (!row.expanded) expand(row)
        else go(index + 1)
        return
      case 'ArrowLeft':
        event.preventDefault()
        if (row.expanded) dispatch({ type: 'collapse', id: row.node.id })
        else if (row.parentId !== null) focusRow(row.parentId)
        return
      case 'Enter':
      case ' ':
        event.preventDefault()
        activate(row)
        return
      case '*':
        // Expand every sibling of the focused row, as the ARIA tree pattern suggests.
        event.preventDefault()
        rows.filter((r) => r.parentId === row.parentId).forEach(expand)
        return
      default:
        break
    }
    if (event.key.length === 1 && !event.ctrlKey && !event.metaKey && !event.altKey) {
      const ta = typeAhead.current
      ta.buffer += event.key
      if (ta.timer !== null) clearTimeout(ta.timer)
      ta.timer = setTimeout(() => {
        ta.buffer = ''
      }, TYPE_AHEAD_RESET_MS)
      const match = findTypeAhead(rows, index, ta.buffer)
      if (match >= 0) go(match)
    }
  }

  // A folder that shows "loading" but has no live request lost it to a remount: ask again.
  useEffect(() => {
    for (const row of rows) {
      if (row.expanded && row.child?.status === 'loading' && !aborters.current.has(row.node.id)) {
        load(row.node)
      }
    }
  }, [rows, load])

  // Reveal a deep selection one level per pass: a folder can only be expanded once its parent's
  // children are loaded and visible, and each load changes `rows`, which runs this again.
  useEffect(() => {
    if (revealIds === undefined) return
    for (const id of revealIds) {
      if (revealed.current.has(id)) continue
      const row = rows.find((r) => r.node.id === id)
      if (row === undefined) return
      revealed.current.add(id)
      if (row.node.hasChildren && !row.expanded) expand(row)
      return
    }
    if (selectedId !== null) itemRefs.current.get(selectedId)?.scrollIntoView({ block: 'nearest' })
  }, [revealIds, rows, expand, selectedId])

  const busy = rows.find(
    (r) => r.child?.status === 'loading' || state.more.get(r.node.id) === 'loading',
  )
  const failed = rows.find((r) => r.child?.status === 'error')

  return (
    <div className={className}>
      <ul role="tree" aria-label={label} className="m-0 list-none p-0" onKeyDown={onKeyDown}>
        {rows.map((row) => {
          const isSelected = row.node.id === selectedId && !row.node.more
          const isLoading = row.child?.status === 'loading'
          const hasError = row.child?.status === 'error'
          const isEmpty = row.child?.status === 'loaded' && row.child.items.length === 0
          const moreStatus = row.node.more ? state.more.get(row.node.id) : undefined
          const FolderIcon = row.expanded ? FolderOpen : Folder
          return (
            // Keyboard activation (Enter, Space, arrows) is handled once on the tree (onKeyDown of
            // the <ul>), as the ARIA tree pattern prescribes, not per row.
            // eslint-disable-next-line jsx-a11y/click-events-have-key-events -- see above
            <li
              key={row.node.id}
              ref={(el) => {
                if (el) itemRefs.current.set(row.node.id, el)
                else itemRefs.current.delete(row.node.id)
              }}
              role="treeitem"
              aria-level={row.level}
              aria-posinset={row.posInSet}
              aria-setsize={row.setSize}
              aria-expanded={row.node.hasChildren ? row.expanded : undefined}
              aria-selected={isSelected}
              aria-busy={isLoading || moreStatus === 'loading' || undefined}
              tabIndex={row.node.id === tabStopId ? 0 : -1}
              onFocus={(event) => {
                if (event.target === event.currentTarget)
                  dispatch({ type: 'focus', id: row.node.id })
              }}
              onClick={() => {
                focusRow(row.node.id)
                activate(row)
              }}
              style={{ paddingLeft: `${(row.level - 1) * 1.25 + 0.5}rem` }}
              className={cn(
                'focus-inset flex min-h-(--row-h) cursor-pointer items-center gap-1.5 rounded-md pr-2 text-sm outline-offset-[-2px] select-none hover:bg-muted',
                isSelected && 'bg-secondary text-on-secondary hover:bg-secondary',
              )}
            >
              <ChevronRight
                aria-hidden="true"
                className={cn(
                  'size-4 shrink-0 text-muted-foreground transition-transform duration-150',
                  row.expanded && 'rotate-90',
                  !row.node.hasChildren && 'invisible',
                  isSelected && 'text-on-secondary',
                )}
              />
              {renderLabel ? (
                renderLabel(row.node, { expanded: row.expanded })
              ) : (
                <>
                  {row.node.more ? (
                    <Ellipsis className="size-4 shrink-0" aria-hidden="true" />
                  ) : row.node.hasChildren ? (
                    <FolderIcon className="size-4 shrink-0" aria-hidden="true" />
                  ) : (
                    <File className="size-4 shrink-0" aria-hidden="true" />
                  )}
                  <span className="truncate">{row.node.label}</span>
                </>
              )}
              {isLoading || moreStatus === 'loading' ? (
                <Loader2 className="ml-auto size-4 animate-spin-slow" aria-hidden="true" />
              ) : null}
              {moreStatus === 'error' ? (
                <span className="ml-auto flex items-center gap-1 text-xs text-destructive-text">
                  <TriangleAlert className="size-3.5" aria-hidden="true" />
                  {t('tree.moreFailed')}
                </span>
              ) : null}
              {hasError ? (
                <span className="ml-auto flex items-center gap-1 text-xs text-destructive-text">
                  <TriangleAlert className="size-3.5" aria-hidden="true" />
                  {t('tree.loadFailed', { name: row.node.label })}
                </span>
              ) : null}
              {isEmpty ? (
                <span className="ml-auto text-xs text-muted-foreground">{t('tree.empty')}</span>
              ) : null}
            </li>
          )
        })}
      </ul>
      <p className="sr-only" role="status" aria-live="polite">
        {busy
          ? t('tree.loadingChildren', { name: busy.node.label })
          : failed
            ? t('tree.loadFailed', { name: failed.node.label })
            : ''}
      </p>
    </div>
  )
}

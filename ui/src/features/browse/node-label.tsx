import { File, Folder, FolderOpen, Layers } from 'lucide-react'
import { BytesText } from '@/components/bytes-text'
import { t } from '@/i18n'
import { dataOf } from './tree-nodes'
import type { TreeNodeData } from '@/components/tree/tree-model'

/**
 * One row of the repository tree: icon, name, and the facts a scan needs (size of a file, entry
 * count of a folder, the serving member of a group). Nothing interactive sits inside a row, so the
 * tree stays a single focusable widget.
 */
export function NodeLabel({ node, expanded }: { node: TreeNodeData; expanded: boolean }) {
  const data = dataOf(node)
  if (data?.kind === 'more') {
    return <span className="truncate font-medium">{node.label}</span>
  }
  const entry = data?.kind === 'folder' || data?.kind === 'asset' ? data.entry : null
  const Icon = data?.kind === 'asset' ? File : expanded ? FolderOpen : Folder
  return (
    <>
      <Icon className="size-4 shrink-0" aria-hidden="true" />
      <span className={data?.kind === 'root' ? 'truncate font-mono font-medium' : 'truncate'}>
        {node.label}
      </span>
      {entry?.source_repository ? (
        <span className="inline-flex shrink-0 items-center gap-1 rounded border border-current px-1 text-xs">
          <Layers className="size-3" aria-hidden="true" />
          <span className="sr-only">{t('browse.servedBy')} </span>
          {entry.source_repository}
        </span>
      ) : null}
      {entry?.kind === 'asset' && entry.size_bytes !== null ? (
        <BytesText bytes={entry.size_bytes} className="ml-auto shrink-0 pl-2 text-xs" />
      ) : null}
      {entry?.kind === 'folder' && entry.child_count !== null ? (
        <span className="ml-auto shrink-0 pl-2 text-xs tabular-nums">
          {entry.child_count.toLocaleString()}
        </span>
      ) : null}
    </>
  )
}

import { ArrowDown, ArrowUp, GripVertical, Plus, X } from 'lucide-react'
import { useEffect, useRef, useState, type DragEvent, type KeyboardEvent } from 'react'
import { StatusBadge } from '@/components/status-badge'
import { Button } from '@/components/ui/button'
import { Select } from '@/components/ui/select'
import { t } from '@/i18n'
import { cn } from '@/lib/utils'
import { RepoTypeBadge } from '../repo-meta'

export interface MemberInfo {
  name: string
  repo_type: 'hosted' | 'proxy' | 'group'
  online: boolean
}

export interface MemberOrderListProps {
  members: readonly string[]
  onChange: (next: string[]) => void
  /** Repositories that may be added: same format, not the group itself. Members are filtered out here. */
  candidates: readonly MemberInfo[]
  /** Facts of the repositories by name, for the type and status of each member. */
  known: ReadonlyMap<string, MemberInfo>
}

/** `list` with the item at `from` moved to `to`; the same array when nothing changes. */
export function moveItem<T>(list: readonly T[], from: number, to: number): T[] {
  const next = [...list]
  if (from === to || from < 0 || to < 0 || from >= list.length || to >= list.length) return next
  const [item] = next.splice(from, 1)
  next.splice(to, 0, item as T)
  return next
}

/**
 * The ordered members of a group repository. The order is the resolution order: the first
 * member that has a path serves it, so the order is the content of this field and has to be
 * easy to change by every means: drag a row by its handle with a pointer; with the keyboard
 * press Space or Enter on the handle to pick the row up, the arrow keys to move it, Space or
 * Enter to drop it and Escape to put it back; or use the explicit up and down buttons, which
 * also serve touch screens. Every move is announced in a live region.
 */
export function MemberOrderList({ members, onChange, candidates, known }: MemberOrderListProps) {
  const [grabbed, setGrabbed] = useState<{ name: string; origin: number } | null>(null)
  const [dragFrom, setDragFrom] = useState<number | null>(null)
  const [dragOver, setDragOver] = useState<number | null>(null)
  const [announcement, setAnnouncement] = useState('')
  const [choice, setChoice] = useState('')
  const handles = useRef(new Map<string, HTMLButtonElement>())
  const focusAfterMove = useRef<string | null>(null)

  // A row that moved is a different DOM position; give its handle the focus back so a keyboard
  // user keeps moving the same row.
  useEffect(() => {
    if (focusAfterMove.current !== null) {
      handles.current.get(focusAfterMove.current)?.focus()
      focusAfterMove.current = null
    }
  }, [members])

  const available = candidates.filter((c) => !members.includes(c.name))
  const selected = available.some((c) => c.name === choice) ? choice : (available[0]?.name ?? '')

  function announce(name: string, index: number): void {
    setAnnouncement(t('repoForm.group.moved', { name, position: index + 1, count: members.length }))
  }

  function move(from: number, to: number, keepFocus: boolean): void {
    const name = members[from]
    if (name === undefined || to < 0 || to >= members.length || from === to) return
    if (keepFocus) focusAfterMove.current = name
    onChange(moveItem(members, from, to))
    announce(name, to)
  }

  function onHandleKey(event: KeyboardEvent<HTMLButtonElement>, name: string, index: number): void {
    const isGrabbed = grabbed?.name === name
    if (event.key === ' ' || event.key === 'Enter') {
      event.preventDefault()
      if (isGrabbed) {
        setGrabbed(null)
        setAnnouncement(
          t('repoForm.group.dropped', { name, position: index + 1, count: members.length }),
        )
      } else {
        setGrabbed({ name, origin: index })
        setAnnouncement(
          t('repoForm.group.grabbed', { name, position: index + 1, count: members.length }),
        )
      }
      return
    }
    if (!isGrabbed) return
    if (event.key === 'ArrowUp' || event.key === 'ArrowDown') {
      event.preventDefault()
      move(index, index + (event.key === 'ArrowUp' ? -1 : 1), true)
    } else if (event.key === 'Escape') {
      // Escape puts the row back; it must not also close a dialog around the form.
      event.preventDefault()
      event.stopPropagation()
      focusAfterMove.current = name
      onChange(moveItem(members, index, grabbed.origin))
      setGrabbed(null)
      setAnnouncement(t('repoForm.group.cancelled', { name }))
    }
  }

  function onDrop(event: DragEvent<HTMLLIElement>, to: number): void {
    event.preventDefault()
    if (dragFrom !== null) move(dragFrom, to, false)
    setDragFrom(null)
    setDragOver(null)
  }

  return (
    <div className="space-y-3">
      {members.length === 0 ? (
        <p className="rounded-md border border-dashed border-input px-3 py-4 text-sm text-muted-foreground">
          {t('repoForm.group.empty')}
        </p>
      ) : (
        <ol aria-label={t('repoForm.group.listLabel')} className="m-0 list-none space-y-1 p-0">
          {members.map((name, index) => {
            const info = known.get(name)
            const isGrabbed = grabbed?.name === name
            return (
              <li
                key={name}
                draggable
                onDragStart={(event) => {
                  setDragFrom(index)
                  event.dataTransfer.effectAllowed = 'move'
                  event.dataTransfer.setData('text/plain', name)
                }}
                onDragOver={(event) => {
                  if (dragFrom === null) return
                  event.preventDefault()
                  setDragOver(index)
                }}
                onDrop={(event) => onDrop(event, index)}
                onDragEnd={() => {
                  setDragFrom(null)
                  setDragOver(null)
                }}
                data-dragging={dragFrom === index || undefined}
                className={cn(
                  'flex flex-wrap items-center gap-2 rounded-md border bg-card px-2 py-1.5',
                  isGrabbed ? 'border-2 border-ring' : 'border-border',
                  dragOver === index && dragFrom !== index && 'border-t-4 border-t-accent',
                  dragFrom === index && 'opacity-60',
                )}
              >
                <button
                  type="button"
                  ref={(el) => {
                    if (el) handles.current.set(name, el)
                    else handles.current.delete(name)
                  }}
                  aria-label={t('repoForm.group.handle', { name })}
                  aria-pressed={isGrabbed}
                  aria-describedby="member-order-help"
                  onKeyDown={(event) => onHandleKey(event, name, index)}
                  onBlur={(event) => {
                    // A blur with a target is the user leaving the handle; a blur without one is
                    // the browser re-attaching the row after a move, which must not drop it.
                    if (isGrabbed && event.relatedTarget !== null) setGrabbed(null)
                  }}
                  className="flex size-(--control-h) cursor-grab items-center justify-center rounded-md text-muted-foreground hover:bg-muted active:cursor-grabbing"
                >
                  <GripVertical className="size-4" aria-hidden="true" />
                </button>
                <span className="w-6 text-right text-sm text-muted-foreground tabular-nums">
                  <span className="sr-only">{t('repoForm.group.position')} </span>
                  {index + 1}
                </span>
                <span className="min-w-0 flex-1 truncate font-mono text-sm">{name}</span>
                {info ? (
                  <span className="flex items-center gap-1">
                    <RepoTypeBadge type={info.repo_type} />
                    {info.online ? null : (
                      <StatusBadge status="warning" label={t('status.offline')} />
                    )}
                  </span>
                ) : null}
                <span className="flex items-center">
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={t('repoForm.group.moveUp', { name })}
                    disabled={index === 0}
                    onClick={() => move(index, index - 1, true)}
                  >
                    <ArrowUp aria-hidden="true" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={t('repoForm.group.moveDown', { name })}
                    disabled={index === members.length - 1}
                    onClick={() => move(index, index + 1, true)}
                  >
                    <ArrowDown aria-hidden="true" />
                  </Button>
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={t('repoForm.group.remove', { name })}
                    onClick={() => onChange(members.filter((m) => m !== name))}
                  >
                    <X aria-hidden="true" />
                  </Button>
                </span>
              </li>
            )
          })}
        </ol>
      )}
      <p id="member-order-help" className="text-xs text-muted-foreground">
        {t('repoForm.group.help')}
      </p>
      {members.length > 1 ? (
        <p className="text-sm">
          <span className="font-medium">{t('repoForm.group.preview')}</span>{' '}
          <span className="font-mono">{members.join(' > ')}</span>
        </p>
      ) : null}
      <div className="flex flex-wrap items-end gap-2">
        <label className="flex min-w-48 flex-1 flex-col gap-1 text-sm font-medium sm:max-w-sm">
          {t('repoForm.group.addLabel')}
          <Select
            value={selected}
            onChange={(event) => setChoice(event.target.value)}
            disabled={available.length === 0}
          >
            {available.length === 0 ? (
              <option value="">{t('repoForm.group.noCandidates')}</option>
            ) : null}
            {available.map((c) => (
              <option key={c.name} value={c.name}>
                {c.name}
              </option>
            ))}
          </Select>
        </label>
        <Button
          variant="outline"
          disabled={selected === ''}
          onClick={() => {
            onChange([...members, selected])
            setAnnouncement(
              t('repoForm.group.added', { name: selected, position: members.length + 1 }),
            )
            setChoice('')
          }}
        >
          <Plus aria-hidden="true" />
          {t('repoForm.group.add')}
        </Button>
      </div>
      <p className="sr-only" role="status" aria-live="polite">
        {announcement}
      </p>
    </div>
  )
}

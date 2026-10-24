import { CircleAlert } from 'lucide-react'
import { useId, useMemo, useState, type ReactNode } from 'react'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { t } from '@/i18n'
import { cn } from '@/lib/utils'

export interface PickerItem {
  id: string
  label: string
  /** Secondary line: what the item is. */
  description?: string
  /** Not selectable, with the reason shown under it (never a bare greyed out box). */
  disabledReason?: string
  /** Small marker after the label: "Built-in". */
  badge?: ReactNode
}

export interface MultiPickerProps {
  legend: ReactNode
  /** Plural noun for the filter box and its announcement: "privileges". */
  what: string
  items: readonly PickerItem[]
  value: readonly string[]
  onChange: (next: string[]) => void
  helper?: ReactNode
  error?: string
  disabled?: boolean
  className?: string
}

/**
 * A filterable list of checkboxes for choosing several objects (privileges of a role, nested
 * roles, roles of a user). A group of native-like controls instead of a dropdown, so the whole
 * choice stays visible and works the same with keyboard, pointer and screen reader: the group has a
 * legend, every box a visible label, the count is announced politely.
 */
export function MultiPicker({
  legend,
  what,
  items,
  value,
  onChange,
  helper,
  error,
  disabled = false,
  className,
}: MultiPickerProps) {
  const [filter, setFilter] = useState('')
  const base = useId()
  const messageId = `${base}-message`
  const shown = useMemo(() => {
    const needle = filter.trim().toLowerCase()
    return needle === ''
      ? items
      : items.filter(
          (i) =>
            i.label.toLowerCase().includes(needle) ||
            (i.description ?? '').toLowerCase().includes(needle),
        )
  }, [items, filter])
  const selected = new Set(value)

  function toggle(id: string, checked: boolean): void {
    onChange(checked ? [...value.filter((v) => v !== id), id] : value.filter((v) => v !== id))
  }

  return (
    <fieldset
      className={cn('space-y-2', className)}
      aria-describedby={helper || error ? messageId : undefined}
      aria-invalid={error ? true : undefined}
      disabled={disabled}
    >
      <legend className="text-sm font-medium">{legend}</legend>
      <Input
        type="search"
        value={filter}
        onChange={(e) => setFilter(e.target.value)}
        aria-label={t('identity.picker.search', { what })}
        placeholder={t('identity.picker.search', { what })}
      />
      <ul className="max-h-56 divide-y divide-border overflow-y-auto rounded-md border border-input bg-card">
        {items.length === 0 ? (
          <li className="px-3 py-2 text-sm text-muted-foreground">{t('identity.picker.empty')}</li>
        ) : shown.length === 0 ? (
          <li className="px-3 py-2 text-sm text-muted-foreground">{t('identity.picker.none')}</li>
        ) : (
          shown.map((item) => {
            const boxId = `${base}-${item.id}`
            const reasonId = `${boxId}-reason`
            const blocked = item.disabledReason !== undefined
            return (
              <li key={item.id} className="flex items-start gap-3 px-3 py-2">
                <Checkbox
                  id={boxId}
                  className="mt-0.5"
                  checked={selected.has(item.id)}
                  disabled={blocked}
                  aria-describedby={blocked ? reasonId : undefined}
                  onCheckedChange={(checked) => toggle(item.id, checked === true)}
                />
                <div className="min-w-0 flex-1">
                  <Label htmlFor={boxId} className="flex flex-wrap items-center gap-2 leading-5">
                    <span className="font-mono break-all">{item.label}</span>
                    {item.badge}
                  </Label>
                  {item.description ? (
                    <p className="text-xs text-muted-foreground">{item.description}</p>
                  ) : null}
                  {blocked ? (
                    <p id={reasonId} className="text-xs text-muted-foreground">
                      {item.disabledReason}
                    </p>
                  ) : null}
                </div>
              </li>
            )
          })
        )}
      </ul>
      <p role="status" className="text-xs text-muted-foreground">
        {t('identity.picker.selected', { count: value.length })}
      </p>
      <div id={messageId} className="space-y-1 empty:hidden">
        {helper ? <p className="text-xs text-muted-foreground">{helper}</p> : null}
        {error ? (
          <p className="flex items-start gap-1 text-xs font-medium text-destructive-text">
            <CircleAlert className="mt-px size-3.5 shrink-0" aria-hidden="true" />
            <span>{error}</span>
          </p>
        ) : null}
      </div>
    </fieldset>
  )
}

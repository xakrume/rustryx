import { Search, X } from 'lucide-react'
import { useEffect, useRef, useState, type ReactNode } from 'react'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Select } from '@/components/ui/select'
import { useDebouncedValue } from '@/hooks/use-debounced-value'
import { t } from '@/i18n'

export type FilterDef =
  | { id: string; type: 'search'; label: string; placeholder?: string }
  | {
      id: string
      type: 'select'
      label: string
      options: readonly { value: string; label: string }[]
    }

export interface FilterBarProps {
  filters: readonly FilterDef[]
  /** Current filter values by id; an absent or empty value means "no filter". */
  values: Readonly<Record<string, string | undefined>>
  /** Called with the new value, or null to remove the filter. */
  onChange: (id: string, value: string | null) => void
  onClear: () => void
  /** Extra controls on the right: column chooser, density, refresh. */
  trailing?: ReactNode
}

const SEARCH_DEBOUNCE_MS = 300

/**
 * Filters of a server side table: they only produce query values, the server filters. Search
 * text is debounced so typing does not fire a request per key. Active filters show as chips
 * with a remove button each, and one "Clear filters" resets all of them.
 */
export function FilterBar({ filters, values, onChange, onClear, trailing }: FilterBarProps) {
  const active = filters.filter((f) => (values[f.id] ?? '') !== '')
  return (
    <div className="space-y-2">
      <div role="search" aria-label={t('table.filters')} className="flex flex-wrap items-end gap-2">
        {filters.map((filter) =>
          filter.type === 'search' ? (
            <SearchFilter
              key={filter.id}
              filter={filter}
              value={values[filter.id] ?? ''}
              onChange={(v) => onChange(filter.id, v === '' ? null : v)}
            />
          ) : (
            <label key={filter.id} className="flex flex-col gap-1 text-xs font-medium">
              <span>{filter.label}</span>
              <Select
                value={values[filter.id] ?? ''}
                className="min-w-36"
                onChange={(e) => onChange(filter.id, e.target.value === '' ? null : e.target.value)}
              >
                <option value="">{t('common.any')}</option>
                {filter.options.map((o) => (
                  <option key={o.value} value={o.value}>
                    {o.label}
                  </option>
                ))}
              </Select>
            </label>
          ),
        )}
        <div className="ml-auto flex items-center gap-1">{trailing}</div>
      </div>
      {active.length > 0 ? (
        <ul aria-label={t('table.activeFilters')} className="flex flex-wrap items-center gap-2">
          {active.map((filter) => {
            const raw = values[filter.id] ?? ''
            const shown =
              filter.type === 'select'
                ? (filter.options.find((o) => o.value === raw)?.label ?? raw)
                : raw
            return (
              <li
                key={filter.id}
                className="inline-flex items-center gap-1 rounded-md border border-border bg-muted py-0.5 pr-0.5 pl-2 text-xs"
              >
                <span className="text-muted-foreground">{filter.label}:</span>
                <span className="font-medium">{shown}</span>
                <Button
                  variant="ghost"
                  size="icon"
                  className="size-6"
                  aria-label={t('table.removeFilter', { label: filter.label })}
                  onClick={() => onChange(filter.id, null)}
                >
                  <X aria-hidden="true" />
                </Button>
              </li>
            )
          })}
          <li>
            <Button variant="link" size="sm" onClick={onClear}>
              {t('table.clearFilters')}
            </Button>
          </li>
        </ul>
      ) : null}
    </div>
  )
}

function SearchFilter({
  filter,
  value,
  onChange,
}: {
  filter: Extract<FilterDef, { type: 'search' }>
  value: string
  onChange: (value: string) => void
}) {
  const [local, setLocal] = useState(value)
  const debounced = useDebouncedValue(local, SEARCH_DEBOUNCE_MS)
  const lastEmitted = useRef(value)

  // Follow the parent only when it changed the value itself (Clear filters, a chip removed),
  // not when it merely echoes what this input emitted: otherwise a slow echo would overwrite
  // characters typed in the meantime.
  useEffect(() => {
    if (value !== lastEmitted.current) {
      lastEmitted.current = value
      setLocal(value)
    }
  }, [value])

  useEffect(() => {
    if (debounced !== lastEmitted.current) {
      lastEmitted.current = debounced
      onChange(debounced)
    }
  }, [debounced, onChange])

  return (
    <label className="flex min-w-48 flex-1 flex-col gap-1 text-xs font-medium sm:max-w-xs">
      <span>{filter.label}</span>
      <span className="relative">
        <Search
          className="pointer-events-none absolute top-1/2 left-2 size-4 -translate-y-1/2 text-muted-foreground"
          aria-hidden="true"
        />
        <Input
          type="search"
          value={local}
          placeholder={filter.placeholder}
          className="pl-8"
          onChange={(e) => setLocal(e.target.value)}
        />
      </span>
    </label>
  )
}

import { CircleAlert, Plus, Trash2 } from 'lucide-react'
import { useId } from 'react'
import { Button } from '@/components/ui/button'
import { Checkbox } from '@/components/ui/checkbox'
import { Label } from '@/components/ui/label'
import { Select } from '@/components/ui/select'
import { t, type MessageKey } from '@/i18n'
import { SCOPE_ACTIONS, withImplied, type ScopePreset, type ScopeRow } from './scopes'

const PRESETS: readonly { value: ScopePreset; label: MessageKey; hint: MessageKey }[] = [
  { value: 'full', label: 'tokens.create.scopeFull', hint: 'tokens.create.scopeFullHint' },
  { value: 'read', label: 'tokens.create.scopeRead', hint: 'tokens.create.scopeReadHint' },
  { value: 'publish', label: 'tokens.create.scopePublish', hint: 'tokens.create.scopePublishHint' },
  { value: 'custom', label: 'tokens.create.scopeCustom', hint: 'tokens.create.scopeCustomHint' },
]

const MAX_ROWS = 64

export interface ScopePickerProps {
  preset: ScopePreset
  onPresetChange: (preset: ScopePreset) => void
  rows: readonly ScopeRow[]
  onRowsChange: (rows: ScopeRow[]) => void
  repositories: readonly string[]
  error?: string
}

/**
 * What a token may do: the rights of its owner (no restriction), a read only or a publish preset,
 * or custom rows of repository and actions. A scope can only narrow the owner's rights. Choosing
 * write keeps read and browse selected, visibly, because publishing includes reading.
 */
export function ScopePicker({
  preset,
  onPresetChange,
  rows,
  onRowsChange,
  repositories,
  error,
}: ScopePickerProps) {
  const base = useId()
  const nextKey = rows.reduce((max, row) => Math.max(max, row.key), 0) + 1

  function patch(key: number, change: Partial<ScopeRow>): void {
    onRowsChange(rows.map((row) => (row.key === key ? { ...row, ...change } : row)))
  }

  return (
    <fieldset className="space-y-2">
      <legend className="text-sm font-medium">{t('tokens.create.scopes')}</legend>
      {PRESETS.map((option) => (
        <div key={option.value} className="flex items-start gap-2">
          <input
            type="radio"
            id={`${base}-${option.value}`}
            name={`${base}-preset`}
            className="mt-1 size-4 accent-accent"
            checked={preset === option.value}
            aria-describedby={`${base}-${option.value}-hint`}
            onChange={() => onPresetChange(option.value)}
          />
          <div>
            <Label htmlFor={`${base}-${option.value}`}>{t(option.label)}</Label>
            <p id={`${base}-${option.value}-hint`} className="text-xs text-muted-foreground">
              {t(option.hint)}
            </p>
          </div>
        </div>
      ))}
      {preset === 'custom' ? (
        <div className="space-y-2 pt-1">
          <p className="text-sm font-medium">{t('tokens.create.rows')}</p>
          <ol className="space-y-2">
            {rows.map((row, index) => (
              <li key={row.key} className="rounded-md border border-border p-3">
                <div className="flex items-end gap-2">
                  <div className="min-w-0 flex-1 space-y-1">
                    <Label htmlFor={`${base}-row-${row.key}`} className="text-xs">
                      {t('tokens.create.repository')}
                    </Label>
                    <Select
                      id={`${base}-row-${row.key}`}
                      value={row.repository}
                      onChange={(e) => patch(row.key, { repository: e.target.value })}
                    >
                      <option value="">{t('common.none')}</option>
                      <option value="*">{t('tokens.create.repositoryAll')}</option>
                      {repositories.map((name) => (
                        <option key={name} value={name}>
                          {name}
                        </option>
                      ))}
                    </Select>
                  </div>
                  {rows.length > 1 ? (
                    <Button
                      variant="ghost"
                      size="icon"
                      aria-label={t('tokens.create.removeRow', { number: index + 1 })}
                      onClick={() => onRowsChange(rows.filter((r) => r.key !== row.key))}
                    >
                      <Trash2 aria-hidden="true" />
                    </Button>
                  ) : null}
                </div>
                <fieldset
                  className="mt-2 flex flex-wrap gap-x-5 gap-y-1"
                  aria-label={t('tokens.create.actionsFor', { number: index + 1 })}
                >
                  {SCOPE_ACTIONS.map((action) => {
                    const impliedByWrite =
                      row.actions.includes('write') && (action === 'browse' || action === 'read')
                    return (
                      <div key={action} className="flex items-center gap-2">
                        <Checkbox
                          id={`${base}-row-${row.key}-${action}`}
                          checked={row.actions.includes(action)}
                          disabled={impliedByWrite}
                          onCheckedChange={(checked) =>
                            patch(row.key, {
                              actions: withImplied(
                                checked === true
                                  ? [...row.actions, action]
                                  : row.actions.filter((a) => a !== action),
                              ),
                            })
                          }
                        />
                        <Label htmlFor={`${base}-row-${row.key}-${action}`}>
                          {t(`identity.action.${action}`)}
                        </Label>
                      </div>
                    )
                  })}
                </fieldset>
                {row.actions.includes('write') ? (
                  <p className="mt-1 text-xs text-muted-foreground">
                    {t('tokens.create.pushImpliesPull')}
                  </p>
                ) : null}
              </li>
            ))}
          </ol>
          <Button
            variant="outline"
            size="sm"
            disabled={rows.length >= MAX_ROWS}
            onClick={() =>
              onRowsChange([...rows, { key: nextKey, repository: '', actions: ['browse', 'read'] }])
            }
          >
            <Plus aria-hidden="true" />
            {t('tokens.create.addRow')}
          </Button>
        </div>
      ) : null}
      {error ? (
        <p className="flex items-start gap-1 text-xs font-medium text-destructive-text">
          <CircleAlert className="mt-px size-3.5 shrink-0" aria-hidden="true" />
          <span>{error}</span>
        </p>
      ) : null}
    </fieldset>
  )
}

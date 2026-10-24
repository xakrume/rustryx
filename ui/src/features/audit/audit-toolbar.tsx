import { useId } from 'react'
import { FilterBar } from '@/components/data-table'
import { Checkbox } from '@/components/ui/checkbox'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Select } from '@/components/ui/select'
import { t } from '@/i18n'
import { AUDIT_KINDS } from './api'
import { isoToLocalInput, localInputToIso } from './time-input'

export interface AuditToolbarProps {
  values: Readonly<Record<string, string | undefined>>
  onChange: (id: string, value: string | null) => void
  onClear: () => void
}

/**
 * The filters of the audit log: a time range (presets or a custom range), the kinds as a group of
 * checkboxes, and the text filters and the outcome in the shared filter bar. Every value lives in
 * the address bar, so a filtered view can be shared and survives a reload.
 */
export function AuditToolbar({ values, onChange, onClear }: AuditToolbarProps) {
  const base = useId()
  const selected = (values.kind ?? '').split(',').filter(Boolean)
  const custom = values.range === 'custom'

  function toggleKind(kind: string, checked: boolean): void {
    const next = checked ? [...selected, kind] : selected.filter((k) => k !== kind)
    onChange(
      'kind',
      next.length === 0 ? null : AUDIT_KINDS.filter((k) => next.includes(k)).join(','),
    )
  }

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-end gap-x-4 gap-y-3">
        <div className="space-y-1">
          <Label htmlFor={`${base}-range`} className="text-xs">
            {t('auditLog.filters.range')}
          </Label>
          <Select
            id={`${base}-range`}
            value={values.range ?? ''}
            className="min-w-44"
            onChange={(e) => onChange('range', e.target.value === '' ? null : e.target.value)}
          >
            <option value="">{t('auditLog.filters.rangeAny')}</option>
            <option value="hour">{t('auditLog.filters.rangeHour')}</option>
            <option value="day">{t('auditLog.filters.rangeDay')}</option>
            <option value="week">{t('auditLog.filters.rangeWeek')}</option>
            <option value="month">{t('auditLog.filters.rangeMonth')}</option>
            <option value="custom">{t('auditLog.filters.rangeCustom')}</option>
          </Select>
        </div>
        {custom ? (
          <>
            <div className="space-y-1">
              <Label htmlFor={`${base}-from`} className="text-xs">
                {t('auditLog.filters.from')}
              </Label>
              <Input
                id={`${base}-from`}
                type="datetime-local"
                value={isoToLocalInput(values.from)}
                onChange={(e) => onChange('from', localInputToIso(e.target.value))}
              />
            </div>
            <div className="space-y-1">
              <Label htmlFor={`${base}-to`} className="text-xs">
                {t('auditLog.filters.to')}
              </Label>
              <Input
                id={`${base}-to`}
                type="datetime-local"
                value={isoToLocalInput(values.to)}
                aria-describedby={`${base}-custom-help`}
                onChange={(e) => onChange('to', localInputToIso(e.target.value))}
              />
            </div>
            <p id={`${base}-custom-help`} className="pb-2 text-xs text-muted-foreground">
              {t('auditLog.filters.customHelper')}
            </p>
          </>
        ) : null}
        <fieldset className="space-y-1">
          <legend className="text-xs font-medium">{t('auditLog.filters.kind')}</legend>
          <div className="flex flex-wrap gap-x-4 gap-y-1 pt-1">
            {AUDIT_KINDS.map((kind) => (
              <div key={kind} className="flex items-center gap-2">
                <Checkbox
                  id={`${base}-kind-${kind}`}
                  checked={selected.includes(kind)}
                  onCheckedChange={(checked) => toggleKind(kind, checked === true)}
                />
                <Label htmlFor={`${base}-kind-${kind}`} className="text-sm font-normal">
                  {t(`auditLog.kinds.${kind}`)}
                </Label>
              </div>
            ))}
          </div>
        </fieldset>
      </div>
      <FilterBar
        filters={[
          { id: 'actor', type: 'search', label: t('auditLog.filters.actor') },
          { id: 'action', type: 'search', label: t('auditLog.filters.action') },
          { id: 'target', type: 'search', label: t('auditLog.filters.target') },
          { id: 'repository', type: 'search', label: t('auditLog.filters.repository') },
          { id: 'project', type: 'search', label: t('auditLog.filters.project') },
          { id: 'request_id', type: 'search', label: t('auditLog.filters.requestId') },
          {
            id: 'outcome',
            type: 'select',
            label: t('auditLog.filters.outcome'),
            options: [
              { value: 'success', label: t('auditLog.outcome.success') },
              { value: 'failure', label: t('auditLog.outcome.failure') },
            ],
          },
        ]}
        values={values}
        onChange={onChange}
        onClear={onClear}
      />
    </div>
  )
}

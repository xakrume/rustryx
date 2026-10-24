import { useQuery } from '@tanstack/react-query'
import { Search, X } from 'lucide-react'
import { useEffect, useId, useState, type FormEvent } from 'react'
import { FormField } from '@/components/form/form-field'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Select } from '@/components/ui/select'
import { formatLabel } from '@/features/repositories/repo-meta'
import { repositoryNamesQuery } from '@/features/repositories/api'
import { t } from '@/i18n'
import {
  ASSET_SORTS,
  COMPONENT_SORTS,
  isChecksum,
  type SearchCriteria,
  type SearchKind,
} from './api'

const FORMATS = ['maven2', 'npm', 'pypi', 'docker', 'oci', 'helm', 'raw', 'pub'] as const

interface FieldLabels {
  group: string
  name: string
  version: string
  checksum: string
  checksumHelp: string
  note?: string
}

/** Format-specific names for the same criteria (inventory 7.3). Other formats keep the generic labels. */
export function fieldLabels(format: string): FieldLabels {
  if (format === 'maven2') {
    return {
      group: t('search.maven.group'),
      name: t('search.maven.name'),
      version: t('search.maven.version'),
      checksum: t('search.checksum'),
      checksumHelp: t('search.checksumHelp'),
      note: t('search.maven.note'),
    }
  }
  if (format === 'docker' || format === 'oci') {
    return {
      group: t('search.image.namespace'),
      name: t('search.image.name'),
      version: t('search.image.tag'),
      checksum: t('search.image.digest'),
      checksumHelp: t('search.image.digestHelp'),
      note: t('search.image.note'),
    }
  }
  return {
    group: t('search.group'),
    name: t('search.name'),
    version: t('search.version'),
    checksum: t('search.checksum'),
    checksumHelp: t('search.checksumHelp'),
  }
}

/** The chip name for one criterion, using the format-specific label when one applies. */
export function criterionLabel(field: keyof SearchCriteria, format: string | undefined): string {
  const labels = fieldLabels(format ?? '')
  switch (field) {
    case 'group':
      return labels.group
    case 'name':
      return labels.name
    case 'version':
      return labels.version
    case 'checksum':
      return labels.checksum
    case 'q':
      return t('search.keyword')
    case 'format':
      return t('search.format')
    case 'repository':
      return t('search.repository')
    case 'path':
      return t('search.path')
    default:
      return t('search.sort')
  }
}

export interface CriteriaFormProps {
  criteria: SearchCriteria
  /** Inline hint under the keyword field, from a failed search (query too short). */
  keywordHint?: string
  onSearch: (next: SearchCriteria) => void
  onReset: () => void
}

type Draft = Record<Exclude<keyof SearchCriteria, 'kind'>, string> & { kind: SearchKind }

const toDraft = (c: SearchCriteria): Draft => ({
  kind: c.kind,
  q: c.q ?? '',
  format: c.format ?? '',
  repository: c.repository ?? '',
  group: c.group ?? '',
  name: c.name ?? '',
  version: c.version ?? '',
  checksum: c.checksum ?? '',
  path: c.path ?? '',
  sort: c.sort ?? '',
})

function fromDraft(d: Draft): SearchCriteria {
  const value = (v: string): string | undefined => (v.trim() === '' ? undefined : v.trim())
  return {
    kind: d.kind,
    q: value(d.q),
    format: value(d.format),
    repository: value(d.repository),
    group: value(d.group),
    name: value(d.name),
    version: value(d.version),
    checksum: value(d.checksum)?.toLowerCase(),
    path: d.kind === 'assets' ? value(d.path) : undefined,
    sort: value(d.sort),
  }
}

/**
 * The criteria of a search. The form sends nothing while you type: a search is a heavy query and
 * needs a criterion, so it runs on Enter or the Search button, and the address bar then holds
 * the criteria (a search can be bookmarked and shared). Checksums are matched exactly, so the
 * field says what it accepts and checks the length before asking the server.
 */
export function CriteriaForm({ criteria, keywordHint, onSearch, onReset }: CriteriaFormProps) {
  const [draft, setDraft] = useState<Draft>(() => toDraft(criteria))
  const [checksumError, setChecksumError] = useState<string | undefined>()
  const repoList = useId()
  const names = useQuery(repositoryNamesQuery)
  const key = JSON.stringify(criteria)

  // A criterion removed from the chips, or the back button, changes the address: follow it.
  useEffect(() => {
    setDraft(toDraft(criteria))
    setChecksumError(undefined)
    // `key` stands for the whole criteria object.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key])

  const set = (field: keyof Draft, value: string): void =>
    setDraft((d) => ({ ...d, [field]: value }))
  const sorts = draft.kind === 'assets' ? ASSET_SORTS : COMPONENT_SORTS
  const labels = fieldLabels(draft.format)

  function submit(event: FormEvent): void {
    event.preventDefault()
    const next = fromDraft(draft)
    if (next.checksum !== undefined && !isChecksum(next.checksum)) {
      setChecksumError(t('search.checksumInvalid'))
      return
    }
    setChecksumError(undefined)
    onSearch(next)
  }

  return (
    <form role="search" aria-label={t('search.formLabel')} onSubmit={submit} className="space-y-3">
      <fieldset className="flex flex-wrap items-center gap-x-4 gap-y-1">
        <legend className="sr-only">{t('search.kindLabel')}</legend>
        {(['components', 'assets'] as const).map((kind) => (
          <div key={kind} className="flex items-center gap-2">
            <input
              id={`search-kind-${kind}`}
              type="radio"
              name="search-kind"
              checked={draft.kind === kind}
              onChange={() => setDraft((d) => ({ ...d, kind, sort: '' }))}
              className="size-4 accent-accent"
            />
            <Label htmlFor={`search-kind-${kind}`}>{t(`search.kind.${kind}`)}</Label>
          </div>
        ))}
      </fieldset>

      <div className="grid gap-3 sm:grid-cols-2 lg:grid-cols-4">
        <FormField
          className="sm:col-span-2 lg:col-span-2"
          label={t('search.keyword')}
          helper={keywordHint ?? t('search.keywordHelp')}
          error={undefined}
        >
          {(c) => (
            <span className="relative block">
              <Search
                className="pointer-events-none absolute top-1/2 left-2 size-4 -translate-y-1/2 text-muted-foreground"
                aria-hidden="true"
              />
              <Input
                {...c}
                type="search"
                value={draft.q}
                onChange={(e) => set('q', e.target.value)}
                className="pl-8"
                placeholder={t('search.keywordPlaceholder')}
              />
            </span>
          )}
        </FormField>
        <FormField label={t('search.format')}>
          {(c) => (
            <Select {...c} value={draft.format} onChange={(e) => set('format', e.target.value)}>
              <option value="">{t('common.any')}</option>
              {FORMATS.map((f) => (
                <option key={f} value={f}>
                  {formatLabel(f)}
                </option>
              ))}
            </Select>
          )}
        </FormField>
        <FormField label={t('search.repository')}>
          {(c) => (
            <>
              <Input
                {...c}
                list={repoList}
                value={draft.repository}
                onChange={(e) => set('repository', e.target.value)}
                autoComplete="off"
                spellCheck={false}
              />
              <datalist id={repoList}>
                {(names.data?.items ?? []).map((r) => (
                  <option key={r.id} value={r.name} />
                ))}
              </datalist>
            </>
          )}
        </FormField>
        {labels.note ? (
          <p className="text-sm text-muted-foreground sm:col-span-2 lg:col-span-4">{labels.note}</p>
        ) : null}
        <FormField label={labels.group}>
          {(c) => (
            <Input
              {...c}
              value={draft.group}
              onChange={(e) => set('group', e.target.value)}
              spellCheck={false}
            />
          )}
        </FormField>
        <FormField label={labels.name}>
          {(c) => (
            <Input
              {...c}
              value={draft.name}
              onChange={(e) => set('name', e.target.value)}
              spellCheck={false}
            />
          )}
        </FormField>
        <FormField label={labels.version}>
          {(c) => (
            <Input
              {...c}
              value={draft.version}
              onChange={(e) => set('version', e.target.value)}
              spellCheck={false}
            />
          )}
        </FormField>
        <FormField label={t('search.sort')}>
          {(c) => (
            <Select
              {...c}
              value={draft.sort || 'relevance'}
              onChange={(e) => set('sort', e.target.value)}
            >
              {sorts.map((s) => (
                <option key={s} value={s}>
                  {t(`search.sorts.${s}`)}
                </option>
              ))}
            </Select>
          )}
        </FormField>
        <FormField
          className="lg:col-span-2"
          label={labels.checksum}
          helper={labels.checksumHelp}
          error={checksumError}
        >
          {(c) => (
            <Input
              {...c}
              value={draft.checksum}
              onChange={(e) => set('checksum', e.target.value)}
              className="font-mono"
              spellCheck={false}
              autoComplete="off"
            />
          )}
        </FormField>
        {draft.kind === 'assets' ? (
          <FormField
            className="lg:col-span-2"
            label={t('search.path')}
            helper={t('search.pathHelp')}
          >
            {(c) => (
              <Input
                {...c}
                value={draft.path}
                onChange={(e) => set('path', e.target.value)}
                className="font-mono"
                spellCheck={false}
              />
            )}
          </FormField>
        ) : null}
      </div>

      <div className="flex flex-wrap items-center gap-2">
        <Button type="submit">
          <Search aria-hidden="true" />
          {t('common.search')}
        </Button>
        <Button variant="outline" onClick={onReset}>
          <X aria-hidden="true" />
          {t('search.reset')}
        </Button>
      </div>
    </form>
  )
}

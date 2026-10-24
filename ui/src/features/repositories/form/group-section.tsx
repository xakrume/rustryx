import { useQuery } from '@tanstack/react-query'
import { CircleAlert } from 'lucide-react'
import { useMemo } from 'react'
import { useFormContext } from 'react-hook-form'
import { t } from '@/i18n'
import { repositoryNamesQuery } from '../api'
import { MemberOrderList, type MemberInfo } from './member-order-list'
import { FormSection } from './sections'
import type { RepositoryFormValues } from './values'

/**
 * Members of a group, in resolution order. Only repositories of the group's format can join
 * (the server refuses others), and a group cannot contain itself.
 */
export function GroupSection({ self }: { self: string | null }) {
  const { watch, setValue, formState } = useFormContext<RepositoryFormValues>()
  const format = watch('format')
  const members = watch('members')
  const names = useQuery(repositoryNamesQuery)
  const error = formState.errors.members?.message

  const { candidates, known } = useMemo(() => {
    const all = names.data?.items ?? []
    const info = new Map<string, MemberInfo>(
      all.map((r) => [r.name, { name: r.name, repo_type: r.repo_type, online: r.online }]),
    )
    return {
      known: info,
      candidates: all
        .filter((r) => r.format === format && r.name !== self)
        .map((r) => ({ name: r.name, repo_type: r.repo_type, online: r.online })),
    }
  }, [names.data, format, self])

  return (
    <FormSection title={t('repoForm.group.title')} description={t('repoForm.group.body')}>
      <MemberOrderList
        members={members}
        candidates={candidates}
        known={known}
        onChange={(next) =>
          setValue('members', next, {
            shouldDirty: true,
            shouldTouch: true,
            shouldValidate: formState.isSubmitted,
          })
        }
      />
      {typeof error === 'string' ? (
        <p
          role="alert"
          className="flex items-start gap-1 text-xs font-medium text-destructive-text"
        >
          <CircleAlert className="mt-px size-3.5 shrink-0" aria-hidden="true" />
          <span>{error}</span>
        </p>
      ) : null}
    </FormSection>
  )
}

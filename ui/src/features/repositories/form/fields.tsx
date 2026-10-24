import { useQuery } from '@tanstack/react-query'
import { useMemo } from 'react'
import { useFormContext } from 'react-hook-form'
import {
  blobStoresQuery,
  cleanupPoliciesQuery,
  formatsQuery,
  routingRulesQuery,
  type FormatInfo,
} from '../api'
import { formatLabel } from '../repo-meta'
import { configFields, type ConfigField } from './format-config'
import { GroupSection } from './group-section'
import { ProxySection } from './proxy-section'
import { AccessSection, FormatOptionsSection, PoliciesSection, StorageSection } from './sections'
import type { RepositoryFormValues } from './values'

export function useFormatInfo(format: string): {
  info: FormatInfo | undefined
  fields: ConfigField[]
} {
  const formats = useQuery(formatsQuery)
  const info = formats.data?.items.find((f) => f.format === format)
  const fields = useMemo(() => configFields(info?.config_schema), [info])
  return { info, fields }
}

export interface RepositoryFieldsProps {
  mode: 'create' | 'edit'
  /** The repository being edited: it cannot be its own group member. */
  self: string | null
  hasStoredAuth: boolean
  /** Edit mode: the name of the repository's blob store, shown locked. */
  blobStoreName?: string
}

/**
 * Everything the create wizard (step 2) and the settings page share: the sections that depend
 * on the type of the repository. The lists they pick from (blob stores, cleanup policies,
 * routing rules) are read here, so both forms show the same choices.
 */
export function RepositoryFields({
  mode,
  self,
  hasStoredAuth,
  blobStoreName,
}: RepositoryFieldsProps) {
  const { watch } = useFormContext<RepositoryFormValues>()
  const repoType = watch('repo_type')
  const format = watch('format')
  const { info, fields } = useFormatInfo(format)
  const blobStores = useQuery(blobStoresQuery)
  const policies = useQuery(cleanupPoliciesQuery)
  const rules = useQuery(routingRulesQuery)
  return (
    <div className="space-y-4">
      <StorageSection
        mode={mode}
        blobStores={blobStores.data?.items ?? []}
        format={info}
        lockedBlobStore={blobStoreName}
      />
      <AccessSection />
      {repoType === 'proxy' ? <ProxySection mode={mode} hasStoredAuth={hasStoredAuth} /> : null}
      {repoType === 'group' ? <GroupSection self={self} /> : null}
      <PoliciesSection policies={policies.data?.items ?? []} rules={rules.data?.items ?? []} />
      <FormatOptionsSection fields={fields} label={formatLabel(format)} />
    </div>
  )
}

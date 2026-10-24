import { useQuery } from '@tanstack/react-query'
import { Alert } from '@/components/ui/alert'
import { t } from '@/i18n'
import { providersDetailsQuery, type Provider } from './api'

/**
 * Says where administrators come from: only the admin groups of the configuration file, never a
 * mapping. The groups are read from the authentication configuration (`/auth/providers/details`,
 * administrators only, secrets redacted); when that read fails the note still explains the rule.
 */
export function AdminGroupsNote({ provider }: { provider: Provider }) {
  const details = useQuery(providersDetailsQuery)
  const groups = details.data?.[provider].admin_groups
  return (
    <Alert title={t('mappings.adminGroups.title')}>
      <p>{t('mappings.adminGroups.body')}</p>
      <p className="mt-1">
        {details.isError ? (
          t('mappings.adminGroups.unknown')
        ) : groups === undefined ? (
          t('common.loading')
        ) : groups.length === 0 ? (
          t('mappings.adminGroups.none')
        ) : (
          <>
            {t('mappings.adminGroups.groups')}{' '}
            {groups.map((group, i) => (
              <span key={group}>
                {i > 0 ? ', ' : ''}
                <code className="font-mono break-all">{group}</code>
              </span>
            ))}
          </>
        )}
      </p>
    </Alert>
  )
}

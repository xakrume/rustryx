import { t } from '@/i18n'
import type { Action, PrivilegeGrant } from './api'

interface Summarizable {
  grant: PrivilegeGrant
  actions: readonly Action[]
  selector?: { name: string } | null
}

/** One line that says what a privilege allows: "maven-releases, maven2: browse, read, write". */
export function grantSummary(privilege: Summarizable): string {
  const actions = privilege.actions.map((a) => t(`identity.action.${a}`).toLowerCase()).join(', ')
  const { grant } = privilege
  if (grant.type === 'admin') return t('privileges.summary.admin')
  if (grant.type === 'repository-view') {
    return t('privileges.summary.view', {
      repository: grant.repository === '*' ? t('identity.allRepositories') : grant.repository,
      format: grant.format ?? t('identity.format.any').toLowerCase(),
      actions,
    })
  }
  return t('privileges.summary.selector', {
    repository: grant.repository === '*' ? t('identity.allRepositories') : grant.repository,
    selector: privilege.selector?.name ?? grant.selector_id,
    actions,
  })
}

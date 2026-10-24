import { PageHeader } from '@/components/page-header'
import { Skeleton } from '@/components/ui/skeleton'
import { useMe } from '@/features/auth/use-permissions'
import { t } from '@/i18n'
import { PasswordCard } from './password-card'
import { GroupsCard, IdentityCard, PermissionsCard, RolesCard } from './profile-cards'
import { SessionsCard } from './sessions-card'

/**
 * The own profile: identity, effective roles with their origin (manual, OIDC group, LDAP group),
 * the groups of the last sign in, a permission summary, the password (local accounts) and a way
 * to end every session. It reads `/me`, which the shell already loaded, so there is no spinner.
 */
export function ProfilePage() {
  const me = useMe()
  if (me === undefined) {
    return (
      <>
        <PageHeader title={t('nav.items.profile')} />
        <Skeleton className="h-56 w-full" />
      </>
    )
  }
  const user = me.user
  if (user === null) return null
  return (
    <>
      <PageHeader title={t('nav.items.profile')} description={t('profile.description')} />
      <div className="grid items-start gap-4 lg:grid-cols-2">
        <IdentityCard me={me} user={user} />
        <RolesCard me={me} />
        <GroupsCard me={me} local={user.source === 'local'} />
        <PasswordCard source={user.source} />
        <PermissionsCard me={me} />
        <SessionsCard />
      </div>
    </>
  )
}

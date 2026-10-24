import { useQuery, useQueryClient } from '@tanstack/react-query'
import { Link, useNavigate } from '@tanstack/react-router'
import {
  ArrowLeft,
  KeyRound,
  LogOut,
  Pencil,
  UserCog,
  UserMinus,
  UserPlus,
  Trash2,
} from 'lucide-react'
import { useState, type ReactNode } from 'react'
import { ConfirmDialog } from '@/components/confirm-dialog'
import { ConflictDialog } from '@/components/conflict-dialog'
import { CopyButton } from '@/components/copy-button'
import { ErrorPanel } from '@/components/error-panel'
import { PageHeader } from '@/components/page-header'
import { PrivilegeGate } from '@/components/privilege-gate'
import { RelativeTime } from '@/components/relative-time'
import { StatusBadge } from '@/components/status-badge'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { useMe } from '@/features/auth/use-permissions'
import { ActionConfirmDialog } from '@/features/roles/action-confirm-dialog'
import { OriginBadge, SourceBadge } from '@/features/roles/origin-badge'
import { useUpdateResource } from '@/hooks/use-update-resource'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import {
  patchUser,
  setUserRoles,
  useDeleteUser,
  useSignOutUser,
  userKeys,
  userQuery,
  type UserPatch,
  type UserView,
} from './api'
import { EditProfileDialog } from './edit-profile-dialog'
import { ResetPasswordDialog } from './reset-password-dialog'
import { UserRolesDialog } from './user-roles-dialog'
import { UserTokensCard } from './user-tokens-card'

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="contents">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </div>
  )
}

/** One account: identity, roles with their origin, sign in state, tokens and the admin actions. */
export function UserDetailPage({ id }: { id: string }) {
  const query = useQuery(userQuery(id))
  if (query.isPending) {
    return (
      <>
        <PageHeader title={t('pages.userDetail')} />
        <div aria-busy="true" className="grid gap-4 lg:grid-cols-2">
          <Skeleton className="h-56 w-full" />
          <Skeleton className="h-56 w-full" />
        </div>
      </>
    )
  }
  if (query.isError) {
    return (
      <>
        <PageHeader title={t('pages.userDetail')} />
        <ErrorPanel error={query.error} onRetry={() => void query.refetch()} />
        <p className="mt-3">
          <Link to="/admin/users" className="text-sm text-link underline underline-offset-2">
            {t('users.detail.back')}
          </Link>
        </p>
      </>
    )
  }
  return <UserDetail user={query.data.data} />
}

type Dialog = 'profile' | 'roles' | 'password' | 'disable' | 'signout' | 'delete' | null

function UserDetail({ user }: { user: UserView }) {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const me = useMe()
  const [dialog, setDialog] = useState<Dialog>(null)
  const isSelf = me?.user?.id === user.id
  const external = user.source !== 'local'
  const signOut = useSignOutUser(user.id)
  const remove = useDeleteUser()

  const resourceName = `user ${user.username}`
  const fetchCurrent = () => queryClient.fetchQuery({ ...userQuery(user.id), staleTime: 0 })
  const patch = useUpdateResource<UserView, UserPatch>({
    queryKey: userKeys.detail(user.id),
    resourceName,
    update: (change, etag) => patchUser(user.id, change, etag),
    fetchCurrent,
    invalidate: [userKeys.all],
    copyChanges: (change) => JSON.stringify(change, null, 2),
    onSuccess: (saved) => {
      setDialog(null)
      toast.success(
        saved.enabled
          ? t('users.disable.enabled', { name: saved.username })
          : t('users.disable.done', { name: saved.username }),
      )
    },
  })
  const roles = useUpdateResource<UserView, string[]>({
    queryKey: userKeys.detail(user.id),
    resourceName,
    update: (roleIds, etag) => setUserRoles(user.id, roleIds, etag),
    fetchCurrent,
    invalidate: [userKeys.all, ['roles']],
    copyChanges: (roleIds) => JSON.stringify({ role_ids: roleIds }, null, 2),
    onSuccess: (saved) => {
      setDialog(null)
      toast.success(t('users.roles.saved', { name: saved.username }))
    },
    onError: (error) => toast.apiError(error, t('users.roles.saveFailed')),
  })
  const profile = useUpdateResource<UserView, UserPatch>({
    queryKey: userKeys.detail(user.id),
    resourceName,
    update: (change, etag) => patchUser(user.id, change, etag),
    fetchCurrent,
    invalidate: [userKeys.all],
    copyChanges: (change) => JSON.stringify(change, null, 2),
    onSuccess: (saved) => {
      setDialog(null)
      toast.success(t('users.detail.profileSaved', { name: saved.username }))
    },
  })

  return (
    <>
      <PageHeader
        title={user.username}
        description={t('users.detail.description')}
        actions={
          <PrivilegeGate need="admin">
            {!external ? (
              <Button variant="outline" onClick={() => setDialog('password')}>
                <KeyRound aria-hidden="true" />
                {t('users.actions.resetPassword')}
              </Button>
            ) : null}
            <Button variant="outline" onClick={() => setDialog('signout')}>
              <LogOut aria-hidden="true" />
              {t('users.actions.signOut')}
            </Button>
            {user.enabled ? (
              <Button variant="outline" disabled={isSelf} onClick={() => setDialog('disable')}>
                <UserMinus aria-hidden="true" />
                {t('users.actions.disable')}
              </Button>
            ) : (
              <Button
                variant="outline"
                loading={patch.isPending}
                onClick={() => patch.mutate({ enabled: true })}
              >
                <UserPlus aria-hidden="true" />
                {t('users.actions.enable')}
              </Button>
            )}
            <Button variant="destructive" disabled={isSelf} onClick={() => setDialog('delete')}>
              <Trash2 aria-hidden="true" />
              {t('users.actions.delete')}
            </Button>
          </PrivilegeGate>
        }
      />
      <p className="mb-3">
        <Link
          to="/admin/users"
          className="inline-flex items-center gap-1 text-sm text-link underline underline-offset-2"
        >
          <ArrowLeft className="size-3.5" aria-hidden="true" />
          {t('users.detail.back')}
        </Link>
      </p>
      <div className="mb-4 space-y-2 empty:hidden">
        {isSelf ? <Alert>{t('users.detail.selfNotice')}</Alert> : null}
        {external && user.read_only_reason ? (
          <Alert title={t('users.detail.readOnlyTitle')}>{user.read_only_reason}</Alert>
        ) : null}
      </div>

      <div className="grid gap-4 lg:grid-cols-2">
        <Card>
          <CardHeader className="flex-row items-center justify-between gap-2">
            <CardTitle>{t('users.detail.identity')}</CardTitle>
            {!external && user.editable_fields.includes('display_name') ? (
              <PrivilegeGate need="admin">
                <Button variant="outline" size="sm" onClick={() => setDialog('profile')}>
                  <Pencil aria-hidden="true" />
                  {t('users.detail.editProfile')}
                </Button>
              </PrivilegeGate>
            ) : null}
          </CardHeader>
          <CardContent>
            <dl className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-2 text-sm">
              <Row label={t('users.detail.source')}>
                <SourceBadge source={user.source} />
              </Row>
              <Row label={t('users.detail.status')}>
                <StatusBadge
                  status={user.enabled ? 'ok' : 'neutral'}
                  label={user.enabled ? t('users.status.enabled') : t('users.status.disabled')}
                />
              </Row>
              <Row label={t('users.detail.displayName')}>
                {user.display_name ?? t('identity.noValue')}
              </Row>
              <Row label={t('users.detail.email')}>{user.email ?? t('identity.noValue')}</Row>
              {user.issuer ? (
                <Row label={t('users.detail.issuer')}>
                  <span className="font-mono text-xs break-all">{user.issuer}</span>
                </Row>
              ) : null}
              {user.subject ? (
                <Row label={t('users.detail.subject')}>
                  <span className="inline-flex items-center gap-1">
                    <span className="font-mono text-xs break-all">{user.subject}</span>
                    <CopyButton
                      value={user.subject}
                      what={t('users.detail.subject')}
                      size="icon"
                      className="size-6"
                    />
                  </span>
                </Row>
              ) : null}
              <Row label={t('users.detail.created')}>
                <RelativeTime value={user.created_at} />
              </Row>
              <Row label={t('users.detail.lastLogin')}>
                {user.last_login_at === null ? (
                  t('users.detail.neverSignedIn')
                ) : (
                  <RelativeTime value={user.last_login_at} />
                )}
              </Row>
              {user.tokens_valid_after ? (
                <Row label={t('users.detail.sessionsEnded')}>
                  <RelativeTime value={user.tokens_valid_after} />
                </Row>
              ) : null}
            </dl>
          </CardContent>
        </Card>

        <Card>
          <CardHeader className="flex-row items-center justify-between gap-2">
            <div>
              <CardTitle>{t('users.roles.title')}</CardTitle>
              <p className="text-sm text-muted-foreground">{t('users.roles.description')}</p>
            </div>
            <PrivilegeGate need="admin">
              <Button variant="outline" size="sm" onClick={() => setDialog('roles')}>
                <UserCog aria-hidden="true" />
                {t('users.roles.edit')}
              </Button>
            </PrivilegeGate>
          </CardHeader>
          <CardContent>
            {user.roles.length === 0 ? (
              <p className="text-sm text-muted-foreground">{t('users.roles.none')}</p>
            ) : (
              <ul className="divide-y divide-border text-sm">
                {user.roles.map((entry) => (
                  <li
                    key={`${entry.role.id}-${entry.origin}`}
                    className="flex flex-wrap items-center gap-x-3 gap-y-1 py-2"
                  >
                    <Link
                      to="/admin/roles"
                      search={{ edit: entry.role.id }}
                      className="font-mono font-medium text-link underline-offset-2 hover:underline"
                    >
                      {entry.role.name}
                    </Link>
                    <OriginBadge origin={entry.origin} />
                    {entry.groups.length > 0 ? (
                      <span className="basis-full font-mono text-xs break-all text-muted-foreground">
                        {entry.groups.join(', ')}
                      </span>
                    ) : null}
                  </li>
                ))}
              </ul>
            )}
          </CardContent>
        </Card>

        {external ? (
          <Card>
            <CardHeader>
              <CardTitle>{t('users.detail.groupsSeen')}</CardTitle>
              <p className="text-sm text-muted-foreground">{t('users.detail.groupsSeenHelper')}</p>
            </CardHeader>
            <CardContent>
              {(user.groups_seen ?? []).length === 0 ? (
                <p className="text-sm text-muted-foreground">{t('users.detail.groupsNone')}</p>
              ) : (
                <ul className="space-y-1 font-mono text-xs break-all">
                  {(user.groups_seen ?? []).map((group) => (
                    <li key={group}>{group}</li>
                  ))}
                </ul>
              )}
            </CardContent>
          </Card>
        ) : null}

        <UserTokensCard user={user} />
      </div>

      {dialog === 'profile' ? (
        <EditProfileDialog
          user={user}
          pending={profile.isPending}
          onSave={(change) => profile.mutate(change)}
          onClose={() => setDialog(null)}
        />
      ) : null}
      {dialog === 'roles' ? (
        <UserRolesDialog
          user={user}
          pending={roles.isPending}
          onSave={(ids) => roles.mutate(ids)}
          onClose={() => setDialog(null)}
        />
      ) : null}
      {dialog === 'password' ? (
        <ResetPasswordDialog
          userId={user.id}
          username={user.username}
          onClose={() => setDialog(null)}
        />
      ) : null}
      <ActionConfirmDialog
        open={dialog === 'disable'}
        onOpenChange={(open) => !open && setDialog(null)}
        title={t('users.disable.title', { name: user.username })}
        description={t('users.disable.body')}
        confirmLabel={t('users.disable.confirm')}
        variant="destructive"
        onConfirm={() => patch.mutate({ enabled: false })}
      />
      <ActionConfirmDialog
        open={dialog === 'signout'}
        onOpenChange={(open) => !open && setDialog(null)}
        title={t('users.signOut.title', { name: user.username })}
        description={t('users.signOut.body')}
        confirmLabel={t('users.signOut.confirm')}
        onConfirm={async () => {
          await signOut.mutateAsync()
          toast.success(t('users.signOut.done', { name: user.username }))
        }}
      />
      <ConfirmDialog
        open={dialog === 'delete'}
        onOpenChange={(open) => !open && setDialog(null)}
        kind={t('users.delete.kind')}
        name={user.username}
        description={t('users.delete.description')}
        confirmLabel={t('users.delete.confirm')}
        requireTyping
        onConfirm={async () => {
          await remove.mutateAsync(user.id)
          toast.success(t('users.delete.deleted', { name: user.username }))
          await navigate({ to: '/admin/users' })
        }}
      />
      <ConflictDialog {...patch.conflict} />
      <ConflictDialog {...roles.conflict} />
      <ConflictDialog {...profile.conflict} />
    </>
  )
}

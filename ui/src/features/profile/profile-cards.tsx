import { Check, X } from 'lucide-react'
import type { ReactNode } from 'react'
import { CopyButton } from '@/components/copy-button'
import { RelativeTime } from '@/components/relative-time'
import { StatusBadge } from '@/components/status-badge'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import type { MeView } from '@/features/auth/permissions'
import { OriginBadge, SourceBadge } from '@/features/roles/origin-badge'
import { describeScope } from '@/features/tokens/scopes'
import { t } from '@/i18n'

type User = NonNullable<MeView['user']>

function Row({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="contents">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </div>
  )
}

export function IdentityCard({ me, user }: { me: MeView; user: User }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('profile.identity.title')}</CardTitle>
      </CardHeader>
      <CardContent>
        <dl className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-2 text-sm">
          <Row label={t('profile.identity.name')}>
            {user.display_name ?? t('profile.identity.noName')}
          </Row>
          <Row label={t('profile.identity.username')}>
            <span className="font-mono">{user.username}</span>
          </Row>
          <Row label={t('profile.identity.email')}>{user.email ?? t('identity.noValue')}</Row>
          <Row label={t('profile.identity.source')}>
            <SourceBadge source={user.source} />
          </Row>
          <Row label={t('profile.identity.status')}>
            <StatusBadge
              status={user.enabled ? 'ok' : 'neutral'}
              label={user.enabled ? t('users.status.enabled') : t('users.status.disabled')}
            />
          </Row>
          {user.issuer ? (
            <Row label={t('profile.identity.issuer')}>
              <span className="font-mono text-xs break-all">{user.issuer}</span>
            </Row>
          ) : null}
          {user.subject ? (
            <Row label={t('profile.identity.subject')}>
              <span className="inline-flex items-center gap-1">
                <span className="font-mono text-xs break-all">{user.subject}</span>
                <CopyButton
                  value={user.subject}
                  what={t('profile.identity.subject')}
                  size="icon"
                  className="size-6"
                />
              </span>
            </Row>
          ) : null}
          <Row label={t('profile.identity.created')}>
            <RelativeTime value={user.created_at} />
          </Row>
          <Row label={t('profile.identity.method')}>{t(`profile.method.${me.auth_method}`)}</Row>
          {me.token ? (
            <Row label={t('profile.identity.token')}>
              <span className="font-mono">{me.token.name}</span>
              {me.token.scopes.length > 0 ? (
                <span className="block text-xs text-muted-foreground">
                  {t('profile.identity.tokenScopes')}:{' '}
                  {me.token.scopes.map(describeScope).join(', ')}
                </span>
              ) : null}
            </Row>
          ) : null}
        </dl>
      </CardContent>
    </Card>
  )
}

export function RolesCard({ me }: { me: MeView }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('profile.roles.title')}</CardTitle>
        <CardDescription>{t('profile.roles.description')}</CardDescription>
      </CardHeader>
      <CardContent>
        {me.roles.length === 0 ? (
          <div className="space-y-1 text-sm">
            <p className="font-medium">{t('profile.roles.none')}</p>
            <p className="text-muted-foreground">{t('profile.roles.noneHelp')}</p>
          </div>
        ) : (
          <ul className="divide-y divide-border text-sm">
            {me.roles.map((entry) => (
              <li
                key={`${entry.role.id}-${entry.origin}`}
                className="flex flex-wrap items-center gap-x-3 gap-y-1 py-2"
              >
                <span className="font-mono font-medium">{entry.role.name}</span>
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
  )
}

export function GroupsCard({ me, local }: { me: MeView; local: boolean }) {
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('profile.groups.title')}</CardTitle>
        <CardDescription>{t('profile.groups.description')}</CardDescription>
      </CardHeader>
      <CardContent className="space-y-2">
        {me.groups.length === 0 ? (
          <p className="text-sm text-muted-foreground">
            {local ? t('profile.groups.localNone') : t('profile.groups.none')}
          </p>
        ) : (
          <ul className="space-y-1 font-mono text-xs break-all">
            {me.groups.map((group) => (
              <li key={group}>{group}</li>
            ))}
          </ul>
        )}
        {me.groups_confirmed_at ? (
          <p className="text-xs text-muted-foreground">
            {t('profile.groups.confirmed')}: <RelativeTime value={me.groups_confirmed_at} />
          </p>
        ) : null}
      </CardContent>
    </Card>
  )
}

export function PermissionsCard({ me }: { me: MeView }) {
  const { permissions } = me
  return (
    <Card className="lg:col-span-2">
      <CardHeader>
        <CardTitle>{t('profile.permissions.title')}</CardTitle>
        <CardDescription>{t('profile.permissions.description')}</CardDescription>
      </CardHeader>
      <CardContent className="grid gap-6 md:grid-cols-2">
        <div className="space-y-4 text-sm">
          <div className="space-y-1">
            <h3 className="font-medium">{t('profile.permissions.admin')}</h3>
            <p>
              {permissions.is_admin
                ? t('profile.permissions.adminYes')
                : t('profile.permissions.adminNo')}
            </p>
          </div>
          <div className="space-y-1">
            <h3 className="font-medium">{t('profile.permissions.areas')}</h3>
            {permissions.admin_areas.length === 0 ? (
              <p className="text-muted-foreground">{t('profile.permissions.areasNone')}</p>
            ) : (
              <ul className="flex flex-wrap gap-1">
                {permissions.admin_areas.map((area) => (
                  <li key={area}>
                    <Badge>{t(`profile.permissions.area.${area}`)}</Badge>
                  </li>
                ))}
              </ul>
            )}
          </div>
          <div className="space-y-1">
            <h3 className="font-medium">{t('profile.permissions.capabilities')}</h3>
            <ul className="space-y-1">
              {(Object.keys(permissions.can) as (keyof typeof permissions.can)[]).map((key) => (
                <li key={key} className="flex items-center gap-2">
                  {permissions.can[key] ? (
                    <Check className="size-4 text-success-text" aria-hidden="true" />
                  ) : (
                    <X className="size-4 text-muted-foreground" aria-hidden="true" />
                  )}
                  <span>{t(`profile.permissions.can.${key}`)}</span>
                  <span className="sr-only">
                    :{' '}
                    {permissions.can[key]
                      ? t('profile.permissions.yes')
                      : t('profile.permissions.no')}
                  </span>
                </li>
              ))}
            </ul>
          </div>
        </div>
        <div className="space-y-1 text-sm">
          <h3 className="font-medium">{t('profile.permissions.grants')}</h3>
          {permissions.repository_grants.length === 0 ? (
            <p className="text-muted-foreground">{t('profile.permissions.grantsNone')}</p>
          ) : (
            <ul className="divide-y divide-border">
              {permissions.repository_grants.map((grant) => (
                <li key={`${grant.privilege}-${grant.repository}`} className="space-y-1 py-2">
                  <p className="font-mono text-xs break-all">{grant.privilege}</p>
                  <p className="flex flex-wrap items-center gap-2">
                    <span className="font-mono">
                      {grant.repository === '*' ? t('identity.allRepositories') : grant.repository}
                    </span>
                    {grant.format ? (
                      <span className="font-mono text-xs text-muted-foreground">
                        {grant.format}
                      </span>
                    ) : null}
                    {grant.actions.map((action) => (
                      <Badge key={action}>{t(`identity.action.${action}`)}</Badge>
                    ))}
                  </p>
                  {grant.selector ? (
                    <p className="text-xs text-muted-foreground">
                      {t('profile.permissions.narrowed')}
                    </p>
                  ) : null}
                </li>
              ))}
            </ul>
          )}
        </div>
      </CardContent>
    </Card>
  )
}

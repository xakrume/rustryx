import { StatusBadge } from '@/components/status-badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { t, type MessageKey } from '@/i18n'
import type { SystemStatus } from './api'

type Readiness = SystemStatus['readiness']

const CHECK_LABELS: Record<string, MessageKey> = {
  listening: 'systemStatus.readiness.check.listening',
  draining: 'systemStatus.readiness.check.draining',
  schema: 'systemStatus.readiness.check.schema',
  database: 'systemStatus.readiness.check.database',
}

const ADVISORY_LABELS: Record<string, MessageKey> = {
  redis: 'systemStatus.readiness.advisory.redis',
  oidc: 'systemStatus.readiness.advisory.oidc',
  keycloak: 'systemStatus.readiness.advisory.keycloak',
  ldap: 'systemStatus.readiness.advisory.ldap',
  blob_stores: 'systemStatus.readiness.advisory.blob_stores',
  s3: 'systemStatus.readiness.advisory.s3',
}

const labelOf = (table: Record<string, MessageKey>, name: string): string => {
  const key = table[name]
  return key === undefined ? name : t(key)
}

/**
 * Readiness is the answer of `/readyz` of THIS node, exactly as the server reports it. The word
 * on this card comes from `readiness.status` and from nothing else: a dependency that is down
 * (Redis, Keycloak, LDAP, a blob store) never turns "Ready" into "Not ready" here, because it
 * does not do that for the load balancer either (docs/server.md: PostgreSQL decides readiness).
 */
export function ReadinessCard({ readiness }: { readiness: Readiness }) {
  const ready = readiness.status === 'ready'
  return (
    <Card>
      <CardHeader className="flex-row flex-wrap items-center justify-between gap-2">
        <CardTitle>{t('systemStatus.readiness.title')}</CardTitle>
        <StatusBadge
          status={ready ? 'ok' : 'error'}
          label={ready ? t('systemStatus.readiness.ready') : t('systemStatus.readiness.notReady')}
        />
      </CardHeader>
      <CardContent className="space-y-4 text-sm">
        <p>
          {ready ? t('systemStatus.readiness.readyText') : t('systemStatus.readiness.notReadyText')}
        </p>
        <div>
          <h3 className="mb-1 font-medium">{t('systemStatus.readiness.checks')}</h3>
          <ul className="grid gap-1 sm:grid-cols-2">
            {readiness.checks.map((check) => (
              <li
                key={check.name}
                className="flex items-center justify-between gap-2 rounded-md border border-border px-2 py-1"
              >
                <span>{labelOf(CHECK_LABELS, check.name)}</span>
                <StatusBadge
                  status={check.status === 'ok' ? 'ok' : 'error'}
                  label={
                    check.status === 'ok'
                      ? t('systemStatus.readiness.pass')
                      : t('systemStatus.readiness.fail')
                  }
                />
              </li>
            ))}
          </ul>
        </div>
        {readiness.advisories.length > 0 ? (
          <div>
            <h3 className="mb-1 font-medium">{t('systemStatus.readiness.advisories')}</h3>
            <ul className="grid gap-1 sm:grid-cols-2">
              {readiness.advisories.map((advisory) => (
                <li
                  key={advisory.name}
                  className="flex items-center justify-between gap-2 rounded-md border border-border px-2 py-1"
                >
                  <span>{labelOf(ADVISORY_LABELS, advisory.name)}</span>
                  <StatusBadge
                    status={advisory.status === 'ok' ? 'ok' : 'warning'}
                    label={
                      advisory.status === 'ok'
                        ? t('systemStatus.readiness.advisoryOk')
                        : t('systemStatus.readiness.advisoryDegraded')
                    }
                  />
                </li>
              ))}
            </ul>
            <p className="mt-1 text-xs text-muted-foreground">
              {t('systemStatus.readiness.advisoriesNote')}
            </p>
          </div>
        ) : null}
      </CardContent>
    </Card>
  )
}

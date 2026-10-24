import { KeyRound, LogIn, Package, Settings, ShieldCheck, ShieldX } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { t } from '@/i18n'
import type { AuditKind } from './api'

const CONFIG = {
  login_success: { icon: LogIn, variant: 'neutral' },
  login_failure: { icon: ShieldX, variant: 'destructive' },
  rbac_change: { icon: ShieldCheck, variant: 'info' },
  token_change: { icon: KeyRound, variant: 'info' },
  config_change: { icon: Settings, variant: 'warning' },
  content_change: { icon: Package, variant: 'neutral' },
} as const

/** The kind of an audit event: an icon and its name. An unknown future kind is shown as its raw name. */
export function KindBadge({ kind }: { kind: AuditKind }) {
  const config = (CONFIG as Record<string, (typeof CONFIG)[AuditKind] | undefined>)[kind]
  if (config === undefined) return <Badge>{kind}</Badge>
  const Icon = config.icon
  return (
    <Badge variant={config.variant}>
      <Icon aria-hidden="true" />
      <span>{t(`auditLog.kinds.${kind}`)}</span>
    </Badge>
  )
}

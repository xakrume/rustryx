import { Cloud, Network, UserCheck, KeyRound } from 'lucide-react'
import { Badge } from '@/components/ui/badge'
import { t } from '@/i18n'

export type RoleOrigin = 'manual' | 'oidc' | 'ldap'
export type AccountSource = 'local' | 'oidc' | 'ldap'

const ORIGIN = {
  manual: {
    icon: UserCheck,
    variant: 'neutral',
    label: 'identity.origin.manual',
    hint: 'identity.origin.manualHint',
  },
  oidc: {
    icon: Cloud,
    variant: 'info',
    label: 'identity.origin.oidc',
    hint: 'identity.origin.oidcHint',
  },
  ldap: {
    icon: Network,
    variant: 'info',
    label: 'identity.origin.ldap',
    hint: 'identity.origin.ldapHint',
  },
} as const

/**
 * Where a role comes from: the person who assigned it (manual) or the identity provider group
 * of the last sign in (oidc, ldap). Icon plus text, never color alone; the title repeats the
 * explanation for pointer users, the text is already enough for everyone else.
 */
export function OriginBadge({ origin }: { origin: RoleOrigin }) {
  const config = ORIGIN[origin]
  const Icon = config.icon
  return (
    <Badge variant={config.variant} title={t(config.hint)}>
      <Icon aria-hidden="true" />
      <span>{t(config.label)}</span>
    </Badge>
  )
}

const SOURCE = {
  local: { icon: KeyRound, variant: 'neutral' },
  oidc: { icon: Cloud, variant: 'info' },
  ldap: { icon: Network, variant: 'info' },
} as const

/** The identity source of an account, with an icon next to the word. */
export function SourceBadge({ source }: { source: AccountSource }) {
  const config = SOURCE[source]
  const Icon = config.icon
  return (
    <Badge variant={config.variant}>
      <Icon aria-hidden="true" />
      <span>{t(`identity.source.${source}`)}</span>
    </Badge>
  )
}

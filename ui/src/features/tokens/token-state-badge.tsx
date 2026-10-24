import { StatusBadge, type StatusKind } from '@/components/status-badge'
import { t } from '@/i18n'
import type { ApiTokenView } from './api'

const STATUS: Record<ApiTokenView['state'], StatusKind> = {
  active: 'ok',
  expired: 'warning',
  revoked: 'error',
  disabled: 'neutral',
}

/** The state of a token as icon plus word: active, expired, revoked or disabled. */
export function TokenStateBadge({ token }: { token: Pick<ApiTokenView, 'state'> }) {
  return <StatusBadge status={STATUS[token.state]} label={t(`tokens.states.${token.state}`)} />
}

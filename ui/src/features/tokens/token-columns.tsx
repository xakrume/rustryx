import { Link } from '@tanstack/react-router'
import { Ban } from 'lucide-react'
import type { AnyColumnDef } from '@/components/data-table'
import { RelativeTime } from '@/components/relative-time'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { SourceBadge } from '@/features/roles/origin-badge'
import { t } from '@/i18n'
import type { ApiTokenView } from './api'
import { describeScope, scopeSummary } from './scopes'
import { TokenStateBadge } from './token-state-badge'

const SOON_MS = 14 * 86_400_000

export interface TokenColumnOptions {
  /** The administrator list shows whose token it is, and the kind. */
  admin: boolean
  /** Accessible name of the revoke button of a row. */
  revokeLabel: (token: ApiTokenView) => string
  onRevoke: (token: ApiTokenView) => void
}

function ExpiryCell({ token }: { token: ApiTokenView }) {
  if (token.expires_at === null) {
    return <span className="text-muted-foreground">{t('tokens.neverExpires')}</span>
  }
  const soon = token.state === 'active' && Date.parse(token.expires_at) - Date.now() < SOON_MS
  return (
    <span className="inline-flex items-center gap-2">
      <RelativeTime value={token.expires_at} />
      {soon ? <Badge variant="warning">{t('tokens.expiresSoon')}</Badge> : null}
    </span>
  )
}

/** The columns of a token list; the owner and kind columns only for the administrator view. */
export function tokenColumns({
  admin,
  revokeLabel,
  onRevoke,
}: TokenColumnOptions): AnyColumnDef<ApiTokenView>[] {
  return [
    {
      id: 'name',
      header: t('tokens.columns.name'),
      meta: { label: t('tokens.columns.name'), hideable: false },
      cell: ({ row }) => (
        <span className="inline-flex flex-col leading-tight">
          <span className="font-mono font-medium">{row.original.name}</span>
          <span className="font-mono text-xs text-muted-foreground">
            {row.original.display_prefix}
          </span>
        </span>
      ),
    },
    ...(admin
      ? ([
          {
            id: 'owner',
            header: t('tokens.columns.owner'),
            meta: { label: t('tokens.columns.owner') },
            cell: ({ row }) => (
              <span className="inline-flex items-center gap-2">
                <Link
                  to="/admin/users/$id"
                  params={{ id: row.original.owner.id }}
                  className="font-mono text-link underline-offset-2 hover:underline"
                >
                  {row.original.owner.username}
                </Link>
                <SourceBadge source={row.original.owner.source} />
              </span>
            ),
          },
          {
            id: 'kind',
            header: t('tokens.columns.kind'),
            meta: { label: t('tokens.columns.kind') },
            cell: ({ row }) => t(`tokens.kinds.${row.original.kind}`),
          },
        ] satisfies AnyColumnDef<ApiTokenView>[])
      : []),
    {
      id: 'scopes',
      header: t('tokens.columns.scopes'),
      meta: { label: t('tokens.columns.scopes') },
      cell: ({ row }) => (
        <span
          className="inline-block max-w-56 truncate align-bottom"
          title={row.original.scopes.map(describeScope).join(', ')}
        >
          {scopeSummary(row.original.scopes)}
        </span>
      ),
    },
    {
      id: 'state',
      header: t('tokens.columns.state'),
      meta: { label: t('tokens.columns.state') },
      cell: ({ row }) => <TokenStateBadge token={row.original} />,
    },
    {
      id: 'expires',
      header: t('tokens.columns.expires'),
      meta: { label: t('tokens.columns.expires') },
      cell: ({ row }) => <ExpiryCell token={row.original} />,
    },
    {
      id: 'lastUsed',
      header: t('tokens.columns.lastUsed'),
      meta: { label: t('tokens.columns.lastUsed') },
      cell: ({ row }) =>
        row.original.never_used ? (
          <span className="text-muted-foreground">{t('tokens.neverUsed')}</span>
        ) : (
          <RelativeTime value={row.original.last_used_at} />
        ),
    },
    {
      id: 'created',
      header: t('tokens.columns.created'),
      meta: { label: t('tokens.columns.created') },
      cell: ({ row }) => <RelativeTime value={row.original.created_at} />,
    },
    {
      id: 'actions',
      header: () => <span className="sr-only">{t('tokens.columns.actions')}</span>,
      meta: { label: t('tokens.columns.actions'), hideable: false, align: 'right' },
      cell: ({ row }) =>
        row.original.state === 'revoked' ? null : (
          <Button
            variant="ghost"
            size="icon"
            aria-label={revokeLabel(row.original)}
            onClick={() => onRevoke(row.original)}
          >
            <Ban aria-hidden="true" />
          </Button>
        ),
    },
  ]
}

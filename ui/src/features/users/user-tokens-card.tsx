import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ErrorPanel } from '@/components/error-panel'
import { RelativeTime } from '@/components/relative-time'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { TokenStateBadge } from '@/features/tokens/token-state-badge'
import { t } from '@/i18n'
import { userTokensQuery, type UserView } from './api'

/** The first tokens of the account with a link to the full list, filtered to this owner. */
export function UserTokensCard({ user }: { user: UserView }) {
  const tokens = useQuery(userTokensQuery(user.id))
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('users.tokens.title')}</CardTitle>
        <p className="text-sm text-muted-foreground">
          {t('users.tokens.count', { count: user.token_count })}
        </p>
      </CardHeader>
      <CardContent className="space-y-3">
        {tokens.isPending ? (
          <Skeleton className="h-16 w-full" />
        ) : tokens.isError ? (
          <ErrorPanel error={tokens.error} onRetry={() => void tokens.refetch()} />
        ) : tokens.data.items.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t('users.tokens.none')}</p>
        ) : (
          <ul className="divide-y divide-border text-sm">
            {tokens.data.items.map((token) => (
              <li key={token.id} className="flex flex-wrap items-center gap-x-3 gap-y-1 py-1.5">
                <span className="font-mono font-medium">{token.name}</span>
                <TokenStateBadge token={token} />
                <span className="text-xs text-muted-foreground">
                  {token.never_used ? (
                    t('tokens.neverUsed')
                  ) : (
                    <>
                      {t('tokens.columns.lastUsed')}: <RelativeTime value={token.last_used_at} />
                    </>
                  )}
                </span>
              </li>
            ))}
          </ul>
        )}
        <Link
          to="/admin/tokens"
          search={{ owner_id: user.id, owner: user.username }}
          className="text-sm text-link underline underline-offset-2"
        >
          {t('users.tokens.viewAll')}
        </Link>
      </CardContent>
    </Card>
  )
}

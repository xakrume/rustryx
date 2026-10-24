import { useQuery } from '@tanstack/react-query'
import { ErrorPanel } from '@/components/error-panel'
import { RelativeTime } from '@/components/relative-time'
import { StatusBadge } from '@/components/status-badge'
import { Badge } from '@/components/ui/badge'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { useNow } from '@/hooks/use-now'
import { t } from '@/i18n'
import { parseDate } from '@/lib/format'
import { formatUptime } from './uptime'
import { infoDetailsQuery, nodesQuery, type NodeInfo } from './api'

/** A node that has not reported for this long is shown as stale (the heartbeat is every few seconds). */
const STALE_HEARTBEAT_MS = 60_000

function Row({ node, current, now }: { node: NodeInfo; current: string; now: Date }) {
  const started = parseDate(node.started_at)
  const beat = parseDate(node.last_heartbeat_at)
  const stale = beat !== null && now.getTime() - beat.getTime() > STALE_HEARTBEAT_MS
  return (
    <tr className="border-b border-border last:border-0">
      <th scope="row" className="py-2 pr-3 text-left font-mono font-normal">
        {node.id}{' '}
        {node.id === current ? (
          <Badge variant="info">{t('systemStatus.nodes.thisNode')}</Badge>
        ) : null}
      </th>
      <td className="py-2 pr-3 font-mono">{node.version}</td>
      <td className="py-2 pr-3 whitespace-nowrap tabular-nums">
        {started ? formatUptime((now.getTime() - started.getTime()) / 1000) : '-'}
      </td>
      <td className="py-2 pr-3 whitespace-nowrap">
        <span className="flex flex-col items-start gap-1">
          <RelativeTime value={node.last_heartbeat_at} />
          {stale ? (
            <StatusBadge status="warning" label={t('systemStatus.nodes.staleHeartbeat')} />
          ) : null}
        </span>
      </td>
      <td className="py-2 pr-3">
        {node.draining ? (
          <StatusBadge status="warning" label={t('systemStatus.nodes.draining')} />
        ) : (
          <StatusBadge status="ok" label={t('systemStatus.nodes.serving')} />
        )}
      </td>
      <td className="py-2 font-mono text-xs">
        {node.leader_of.length > 0 ? (
          node.leader_of.join(', ')
        ) : (
          <span className="text-muted-foreground">{t('systemStatus.nodes.noLeader')}</span>
        )}
      </td>
    </tr>
  )
}

/** Version, uptime, heartbeat and singleton leadership of every node, and the schema version. */
export function NodesCard({ currentNode }: { currentNode: string }) {
  const nodes = useQuery(nodesQuery)
  const details = useQuery(infoDetailsQuery)
  const now = useNow()
  const items = nodes.data?.items ?? []
  return (
    <Card>
      <CardHeader className="flex-row flex-wrap items-baseline justify-between gap-2">
        <CardTitle>{t('systemStatus.nodes.title')}</CardTitle>
        <p className="text-sm text-muted-foreground">
          {nodes.data ? t('systemStatus.nodes.count', { count: items.length }) : null}
          {nodes.data && details.data ? ' - ' : null}
          {details.data
            ? t('systemStatus.nodes.schema', { version: details.data.schema_version })
            : null}
        </p>
      </CardHeader>
      <CardContent className="space-y-3">
        {nodes.isError && !nodes.data ? (
          <ErrorPanel
            error={nodes.error}
            onRetry={() => void nodes.refetch()}
            title={t('systemStatus.nodes.loadFailed')}
          />
        ) : !nodes.data ? (
          <Skeleton className="h-24" aria-busy="true" />
        ) : (
          <>
            <div
              role="region"
              aria-label={t('systemStatus.nodes.tableLabel')}
              // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex -- keyboard users must be able to scroll the table
              tabIndex={0}
              className="overflow-x-auto"
            >
              <table className="w-full text-left text-(length:--text-table)">
                <thead>
                  <tr className="border-b border-border text-muted-foreground">
                    <th scope="col" className="py-1 pr-3 font-medium">
                      {t('systemStatus.nodes.columns.id')}
                    </th>
                    <th scope="col" className="py-1 pr-3 font-medium">
                      {t('systemStatus.nodes.columns.version')}
                    </th>
                    <th scope="col" className="py-1 pr-3 font-medium">
                      {t('systemStatus.nodes.columns.uptime')}
                    </th>
                    <th scope="col" className="py-1 pr-3 font-medium">
                      {t('systemStatus.nodes.columns.heartbeat')}
                    </th>
                    <th scope="col" className="py-1 pr-3 font-medium">
                      {t('systemStatus.nodes.columns.state')}
                    </th>
                    <th scope="col" className="py-1 font-medium">
                      {t('systemStatus.nodes.columns.leader')}
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {items.map((node) => (
                    <Row key={node.id} node={node} current={currentNode} now={now} />
                  ))}
                </tbody>
              </table>
            </div>
            {items.length === 1 ? (
              <p className="text-sm text-muted-foreground">{t('systemStatus.nodes.single')}</p>
            ) : null}
          </>
        )}
        {details.isError && !details.data ? (
          <ErrorPanel error={details.error} onRetry={() => void details.refetch()} />
        ) : null}
      </CardContent>
    </Card>
  )
}

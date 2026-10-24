import { useQuery } from '@tanstack/react-query'
import type { ReactNode } from 'react'
import { CodeBlock } from '@/components/code-block'
import { CopyButton } from '@/components/copy-button'
import { ErrorPanel } from '@/components/error-panel'
import { StatusBadge } from '@/components/status-badge'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import { t } from '@/i18n'
import { formatDateTime, parseDate } from '@/lib/format'
import { auditEventQuery, type AuditEvent } from './api'
import { KindBadge } from './kind-badge'

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <div className="contents">
      <dt className="text-muted-foreground">{label}</dt>
      <dd className="min-w-0 break-words">{children}</dd>
    </div>
  )
}

function Copyable({ value, what }: { value: string; what: string }) {
  return (
    <span className="inline-flex items-center gap-1">
      <span className="font-mono text-xs break-all">{value}</span>
      <CopyButton value={value} what={what} size="icon" className="size-6" />
    </span>
  )
}

export interface EventDrawerProps {
  eventId: string
  /** The row the user clicked, shown at once while the event is read again. */
  known?: AuditEvent
  onClose: () => void
}

/**
 * The full record of one audit event in a side drawer: who, what, when, the request id and the
 * JSON detail, each identifier with a copy button. It opens from the address bar (`?event=<id>`),
 * so an event can be linked, and reads the event by id when the row is not in the loaded list.
 */
export function EventDrawer({ eventId, known, onClose }: EventDrawerProps) {
  const query = useQuery({ ...auditEventQuery(eventId), initialData: known })
  const event = query.data
  const at = parseDate(event?.at)
  return (
    <Sheet open onOpenChange={(open) => !open && onClose()}>
      <SheetContent side="right" className="sm:w-[34rem]">
        <div className="flex h-full flex-col overflow-y-auto p-4">
          <SheetHeader>
            <SheetTitle>
              <span className="font-mono">{event?.action ?? t('auditLog.drawer.title')}</span>
            </SheetTitle>
            <SheetDescription>{t('auditLog.drawer.description')}</SheetDescription>
          </SheetHeader>
          {query.isError ? (
            <ErrorPanel
              error={query.error}
              onRetry={() => void query.refetch()}
              title={t('auditLog.drawer.loadFailed')}
            />
          ) : event === undefined ? (
            <div aria-busy="true" className="space-y-2">
              <Skeleton className="h-6 w-full" />
              <Skeleton className="h-40 w-full" />
            </div>
          ) : (
            <div className="space-y-4">
              <dl className="grid grid-cols-[max-content_1fr] gap-x-4 gap-y-2 text-sm">
                <Field label={t('auditLog.drawer.time')}>
                  {at ? (
                    <>
                      <time dateTime={at.toISOString()}>{formatDateTime(at)}</time>
                      <span className="block font-mono text-xs text-muted-foreground">
                        {t('auditLog.drawer.utc')} {at.toISOString()}
                      </span>
                    </>
                  ) : (
                    event.at
                  )}
                </Field>
                <Field label={t('auditLog.drawer.kind')}>
                  <KindBadge kind={event.kind} />
                </Field>
                <Field label={t('auditLog.drawer.outcome')}>
                  <StatusBadge
                    status={event.outcome === 'success' ? 'ok' : 'error'}
                    label={t(`auditLog.outcome.${event.outcome}`)}
                  />
                </Field>
                <Field label={t('auditLog.drawer.actor')}>
                  <span className="font-mono">{event.actor_name}</span>
                </Field>
                {event.actor_id ? (
                  <Field label={t('auditLog.drawer.actorId')}>
                    <Copyable value={event.actor_id} what={t('auditLog.drawer.actorId')} />
                  </Field>
                ) : null}
                <Field label={t('auditLog.drawer.target')}>
                  {event.target ? (
                    <span className="font-mono text-xs break-all">{event.target}</span>
                  ) : (
                    t('auditLog.drawer.noValue')
                  )}
                </Field>
                <Field label={t('auditLog.drawer.clientIp')}>
                  <span className="font-mono text-xs">
                    {event.client_ip ?? t('auditLog.drawer.noValue')}
                  </span>
                </Field>
                <Field label={t('auditLog.drawer.requestId')}>
                  {event.request_id ? (
                    <Copyable value={event.request_id} what={t('auditLog.drawer.requestId')} />
                  ) : (
                    t('auditLog.drawer.noValue')
                  )}
                </Field>
                <Field label={t('auditLog.drawer.eventId')}>
                  <Copyable value={event.id} what={t('auditLog.drawer.eventId')} />
                </Field>
              </dl>
              {Object.keys(event.detail).length === 0 ? (
                <p className="text-sm text-muted-foreground">{t('auditLog.drawer.detailEmpty')}</p>
              ) : (
                <CodeBlock
                  label={t('auditLog.drawer.detail')}
                  value={JSON.stringify(event.detail, null, 2)}
                  wrap
                />
              )}
            </div>
          )}
          <div className="mt-auto flex justify-end pt-4">
            <Button variant="outline" onClick={onClose}>
              {t('auditLog.drawer.close')}
            </Button>
          </div>
        </div>
      </SheetContent>
    </Sheet>
  )
}

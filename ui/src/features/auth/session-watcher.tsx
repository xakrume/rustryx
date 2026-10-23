import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { useEffect, useRef, useState } from 'react'
import { api } from '@/api/client'
import { setCsrfToken } from '@/api/csrf'
import { apiEvents } from '@/api/events'
import { unwrap } from '@/api/request'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { useCountdown } from '@/hooks/use-countdown'
import { t } from '@/i18n'
import { formatDuration } from '@/lib/format'
import { authKeys, sessionQuery, type Session } from './api'

const CHECK_EVERY_MS = 30_000
const WARN_BEFORE_MS = 120_000
const ACTIVITY_EVENTS = ['pointerdown', 'keydown', 'scroll'] as const

/**
 * Keeps an active session alive and warns an idle one (docs/api-conventions.md, 11.2).
 * - While the user is active and less than half of the idle timeout is left, it calls
 *   `POST /auth/session/refresh`. The server also refreshes on normal traffic; this covers a
 *   user who only reads one page.
 * - When the session is about to end and the user has been idle, a dialog counts down and
 *   offers "Stay signed in". The absolute lifetime (8 hours) cannot be extended: there the
 *   dialog only warns.
 * - When the session turns out to be gone, it raises the same event a 401 would, so the one
 *   redirect path (AuthRedirector) handles it.
 */
export function SessionWatcher() {
  const queryClient = useQueryClient()
  const { data: session } = useQuery(sessionQuery)
  const lastActivity = useRef(Date.now())
  const [warnUntil, setWarnUntil] = useState<number | null>(null)

  const refresh = useMutation({
    meta: { silent: true },
    mutationFn: () => unwrap(api.POST('/api/v1/auth/session/refresh')),
    onSuccess: (next: Session) => {
      setCsrfToken(next.csrf_token)
      queryClient.setQueryData(authKeys.session, next)
      setWarnUntil(null)
    },
  })
  const { mutate: refreshSession } = refresh

  useEffect(() => {
    const mark = (): void => {
      lastActivity.current = Date.now()
    }
    ACTIVITY_EVENTS.forEach((name) => window.addEventListener(name, mark, { passive: true }))
    return () => ACTIVITY_EVENTS.forEach((name) => window.removeEventListener(name, mark))
  }, [])

  const authenticated = session?.authenticated === true
  const idleExpires = session?.idle_expires_at ? Date.parse(session.idle_expires_at) : null
  const absoluteExpires = session?.absolute_expires_at
    ? Date.parse(session.absolute_expires_at)
    : null
  const idleMs = (session?.idle_timeout_secs ?? 1800) * 1000

  useEffect(() => {
    if (!authenticated || idleExpires === null) return undefined
    const id = setInterval(() => {
      const now = Date.now()
      const endsAt = Math.min(idleExpires, absoluteExpires ?? Infinity)
      const remaining = endsAt - now
      const active = now - lastActivity.current < idleMs / 2
      if (remaining <= 0) {
        // Ended while the tab slept: verify, and let the 401 path take over.
        apiEvents.emit({ type: 'unauthenticated', path: '/api/v1/auth/session' })
        return
      }
      const idleLimited = idleExpires < (absoluteExpires ?? Infinity)
      if (active && idleLimited && remaining < idleMs / 2) {
        refreshSession()
      } else if (!active && remaining < WARN_BEFORE_MS) {
        setWarnUntil(endsAt)
      } else if (absoluteExpires !== null && absoluteExpires - now < WARN_BEFORE_MS) {
        setWarnUntil(absoluteExpires)
      }
    }, CHECK_EVERY_MS)
    return () => clearInterval(id)
  }, [authenticated, idleExpires, absoluteExpires, idleMs, refreshSession])

  const seconds = useCountdown(warnUntil)
  const canExtend =
    idleExpires !== null && (absoluteExpires === null || idleExpires < absoluteExpires)

  return (
    <Dialog
      open={warnUntil !== null && seconds > 0}
      onOpenChange={(open) => !open && setWarnUntil(null)}
    >
      <DialogContent role="alertdialog">
        <DialogHeader>
          <DialogTitle>{t('shell.session.expiringTitle')}</DialogTitle>
          <DialogDescription>
            {t('shell.session.expiringBody', { time: formatDuration(seconds) })}
          </DialogDescription>
        </DialogHeader>
        <DialogFooter>
          {canExtend ? (
            <Button data-autofocus="" loading={refresh.isPending} onClick={() => refreshSession()}>
              {t('shell.session.stay')}
            </Button>
          ) : (
            <Button data-autofocus="" onClick={() => setWarnUntil(null)}>
              {t('common.close')}
            </Button>
          )}
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

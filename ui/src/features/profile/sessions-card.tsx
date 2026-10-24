import { useQueryClient } from '@tanstack/react-query'
import { Link, useNavigate } from '@tanstack/react-router'
import { LogOut } from 'lucide-react'
import { useState } from 'react'
import { setCsrfToken } from '@/api/csrf'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { ActionConfirmDialog } from '@/features/roles/action-confirm-dialog'
import { t } from '@/i18n'
import { signOutEverywhere } from './api'

/**
 * Ends every session of the account. The server moves `tokens_valid_after` and clears the
 * cookies, so this browser is signed out too: the local state of the old principal is dropped
 * the same way a normal sign out does it, and the person lands on the sign in page.
 */
export function SessionsCard() {
  const queryClient = useQueryClient()
  const navigate = useNavigate()
  const [open, setOpen] = useState(false)
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('profile.sessions.title')}</CardTitle>
        <CardDescription>{t('profile.sessions.description')}</CardDescription>
      </CardHeader>
      <CardContent className="flex flex-wrap items-center gap-3">
        <Button variant="outline" onClick={() => setOpen(true)}>
          <LogOut aria-hidden="true" />
          {t('profile.sessions.action')}
        </Button>
        <Link to="/me/tokens" className="text-sm text-link underline underline-offset-2">
          {t('profile.sessions.tokens')}
        </Link>
      </CardContent>
      <ActionConfirmDialog
        open={open}
        onOpenChange={setOpen}
        title={t('profile.sessions.confirmTitle')}
        description={t('profile.sessions.confirmBody')}
        confirmLabel={t('profile.sessions.confirm')}
        variant="destructive"
        onConfirm={async () => {
          await signOutEverywhere()
          setCsrfToken(null)
          queryClient.clear()
          await navigate({ to: '/login', replace: true })
        }}
      />
    </Card>
  )
}

import '@/lib/zod-setup'
import { useQuery } from '@tanstack/react-query'
import { useRouter } from '@tanstack/react-router'
import { Eye, EyeOff, LogIn } from 'lucide-react'
import { useState } from 'react'
import { z } from 'zod'
import { CopyButton } from '@/components/copy-button'
import { FormField } from '@/components/form/form-field'
import { Form, TextField, useZodForm } from '@/components/form/form'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader } from '@/components/ui/card'
import { Input } from '@/components/ui/input'
import { useCountdown } from '@/hooks/use-countdown'
import { usePageTitle } from '@/hooks/use-page-title'
import { t } from '@/i18n'
import { formatDuration } from '@/lib/format'
import { providersQuery, useLogin } from './api'
import { describeCallbackError, describeLoginFailure, type LoginFailure } from './login-errors'
import { sanitizeReturnTo } from './return-to'

export interface LoginSearch {
  return_to?: string
  error?: string
  request_id?: string
  reason?: 'expired'
}

const schema = z.object({
  username: z.string().trim().min(1, t('auth.login.errors.usernameRequired')),
  password: z.string().min(1, t('auth.login.errors.passwordRequired')),
})

/** Where the SSO button goes: the server's own path, never a foreign URL, plus our return_to. */
function ssoUrl(loginPath: string | null, returnTo: string): string | null {
  if (loginPath === null || !/^\/(?![/\\])/.test(loginPath)) return null
  return `${loginPath}?return_to=${encodeURIComponent(returnTo)}`
}

/**
 * Sign in: the local username and password form plus the "Sign in with Keycloak" redirect.
 * Everything the contract defines for a failed login has a state here: wrong credentials
 * (one generic message), locked out (countdown, submit disabled until it ends), the login
 * service or the identity provider unavailable (local form stays), and a failed SSO callback
 * (`?error=` with the request id). No token is ever stored in the browser: the session is a
 * cookie the server sets.
 */
export function LoginPage({ search }: { search: LoginSearch }) {
  usePageTitle(t('auth.login.title'))
  const router = useRouter()
  const providers = useQuery(providersQuery)
  const login = useLogin()
  const [failure, setFailure] = useState<LoginFailure | null>(null)
  const [showPassword, setShowPassword] = useState(false)
  const returnTo = sanitizeReturnTo(search.return_to)
  const secondsLocked = useCountdown(failure?.retryAt ?? null)
  const locked = secondsLocked > 0

  const form = useZodForm(schema, { defaultValues: { username: '', password: '' } })

  function onSubmit(values: z.infer<typeof schema>): Promise<void> {
    setFailure(null)
    return login.mutateAsync(values).then(
      () => {
        router.history.push(returnTo)
      },
      (error: unknown) => {
        setFailure(describeLoginFailure(error))
        form.resetField('password')
        form.setFocus('password')
      },
    )
  }

  const oidc = providers.data?.oidc
  const sso = oidc?.enabled ? ssoUrl(oidc.login_path, returnTo) : null
  const ssoAvailable = oidc?.available === true && sso !== null
  const provider = oidc?.display_name ?? t('auth.login.ssoDefaultProvider')
  const callbackError = search.error ? describeCallbackError(search.error) : null

  return (
    <main
      id="main-content"
      tabIndex={-1}
      className="flex min-h-dvh items-center justify-center bg-background p-4 outline-none"
    >
      <Card className="w-full max-w-sm">
        <CardHeader className="pb-4">
          <p className="font-mono text-sm font-semibold text-accent">{t('common.appName')}</p>
          <h1
            data-page-title=""
            tabIndex={-1}
            className="text-xl leading-7 font-semibold outline-none"
          >
            {t('auth.login.title')}
          </h1>
          <p className="text-sm text-muted-foreground">{t('auth.login.subtitle')}</p>
        </CardHeader>
        <CardContent className="space-y-4">
          {search.reason === 'expired' ? (
            <Alert variant="info">{t('shell.session.expired')}</Alert>
          ) : null}

          {callbackError ? (
            <Alert variant="destructive">
              <p>{callbackError}</p>
              {search.request_id ? (
                <p className="mt-1 flex flex-wrap items-center gap-1 text-xs">
                  {t('common.requestId')}: <span className="font-mono">{search.request_id}</span>
                  <CopyButton
                    value={search.request_id}
                    what={t('common.requestId')}
                    size="icon"
                    className="size-6"
                    silent
                  />
                </p>
              ) : null}
            </Alert>
          ) : null}

          {failure && (failure.retryAt === null || locked) ? (
            <Alert variant="destructive">
              <p>
                {failure.retryAt !== null
                  ? t('auth.login.errors.lockedOut', { time: formatDuration(secondsLocked) })
                  : failure.message}
              </p>
              {failure.requestId && failure.retryAt === null ? (
                <p className="mt-1 flex flex-wrap items-center gap-1 text-xs">
                  {failure.code ? <span className="font-mono">{failure.code}</span> : null}
                  <span className="font-mono">{failure.requestId}</span>
                  <CopyButton
                    value={failure.requestId}
                    what={t('common.requestId')}
                    size="icon"
                    className="size-6"
                    silent
                  />
                </p>
              ) : null}
            </Alert>
          ) : null}

          <Form form={form} onSubmit={onSubmit} aria-label={t('auth.login.submit')}>
            <TextField<z.infer<typeof schema>>
              name="username"
              label={t('auth.login.username')}
              helper={t('auth.login.usernameHelper')}
              required
              autoComplete="username"
              autoCapitalize="none"
              spellCheck={false}
              // The username box is the one thing this page is for: focusing it saves a keystroke
              // for every returning user and is what screen reader users expect on a sign in page.
              // eslint-disable-next-line jsx-a11y/no-autofocus -- sign in page, single purpose
              autoFocus
            />
            <PasswordField
              show={showPassword}
              onToggle={() => setShowPassword((v) => !v)}
              form={form}
            />
            <Button type="submit" className="w-full" loading={login.isPending} disabled={locked}>
              <LogIn aria-hidden="true" />
              {login.isPending ? t('auth.login.submitting') : t('auth.login.submit')}
            </Button>
          </Form>

          {providers.isError ? (
            <Alert variant="warning">{t('auth.login.providersFailed')}</Alert>
          ) : null}

          {oidc?.enabled ? (
            <div className="space-y-2">
              <div
                className="flex items-center gap-2 text-xs text-muted-foreground"
                aria-hidden="true"
              >
                <span className="h-px flex-1 bg-border" />
                {t('auth.login.or')}
                <span className="h-px flex-1 bg-border" />
              </div>
              {ssoAvailable ? (
                <Button
                  variant="outline"
                  className="w-full"
                  onClick={() => {
                    // A top level navigation, never fetch: the identity provider redirects the page.
                    window.location.assign(sso)
                  }}
                >
                  {t('auth.login.sso', { provider })}
                </Button>
              ) : (
                <>
                  <Button
                    variant="outline"
                    className="w-full"
                    disabled
                    aria-describedby="sso-unavailable"
                  >
                    {t('auth.login.sso', { provider })}
                  </Button>
                  <p id="sso-unavailable" className="text-xs text-muted-foreground">
                    {t('auth.login.ssoUnavailable')}
                  </p>
                </>
              )}
            </div>
          ) : null}

          <p className="text-xs text-muted-foreground">{t('auth.login.breakGlass')}</p>
          {returnTo !== '/' ? (
            <p className="text-xs text-muted-foreground">
              {t('auth.login.returnTo', { path: returnTo })}
            </p>
          ) : null}
        </CardContent>
      </Card>
    </main>
  )
}

function PasswordField({
  show,
  onToggle,
  form,
}: {
  show: boolean
  onToggle: () => void
  form: ReturnType<typeof useZodForm<z.input<typeof schema>, z.output<typeof schema>>>
}) {
  const error = form.formState.errors.password?.message
  return (
    <FormField label={t('auth.login.password')} required error={error}>
      {(control) => (
        <div className="relative">
          <Input
            {...control}
            type={show ? 'text' : 'password'}
            autoComplete="current-password"
            className="pr-10"
            {...form.register('password')}
          />
          <Button
            variant="ghost"
            size="icon"
            className="absolute top-0 right-0"
            aria-label={show ? t('auth.login.hidePassword') : t('auth.login.showPassword')}
            aria-pressed={show}
            onClick={onToggle}
          >
            {show ? <EyeOff aria-hidden="true" /> : <Eye aria-hidden="true" />}
          </Button>
        </div>
      )}
    </FormField>
  )
}

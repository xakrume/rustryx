import { useNavigate, Link, useLocation } from '@tanstack/react-router'
import { LogOut } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuLabel,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { useLogout } from '@/features/auth/api'
import { useMe } from '@/features/auth/use-permissions'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { ACCOUNT_ITEMS } from './nav-config'

const MAX_ROLES_SHOWN = 3

/**
 * Account menu: who you are, which roles you hold (summary, the server decides), account
 * pages, sign out. Anonymous visitors get a Sign in link instead.
 */
export function UserMenu() {
  const me = useMe()
  const navigate = useNavigate()
  const location = useLocation()
  const logout = useLogout()

  if (me === undefined) return null

  if (!me.authenticated || me.user === null) {
    return (
      <Button asChild variant="outline" size="sm">
        <Link to="/login" search={{ return_to: location.href }}>
          {t('shell.userMenu.signIn')}
        </Link>
      </Button>
    )
  }

  const user = me.user
  const name = user.display_name ?? user.username
  const roleNames = me.roles.map((r) => r.role.name)
  const shown = roleNames.slice(0, MAX_ROLES_SHOWN)
  const extra = roleNames.length - shown.length

  function signOut(): void {
    logout.mutate(undefined, {
      onSuccess: ({ redirectUrl }) => {
        if (redirectUrl) window.location.assign(redirectUrl)
        else void navigate({ to: '/login', replace: true })
      },
      onError: (error) => {
        // The local session state is already dropped (useLogout onSettled): the user is out
        // here, and the error is reported so a still valid server session is not hidden.
        toast.apiError(error, t('shell.userMenu.signOut'))
        void navigate({ to: '/login', replace: true })
      },
    })
  }

  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          className="gap-2 px-2"
          aria-label={`${t('shell.userMenu.label')}: ${name}`}
        >
          <span
            aria-hidden="true"
            className="flex size-6 items-center justify-center rounded-full bg-secondary text-xs font-semibold text-on-secondary"
          >
            {name.slice(0, 1).toUpperCase()}
          </span>
          <span className="hidden max-w-32 truncate lg:inline">{name}</span>
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent className="w-64">
        <div className="px-2 py-1.5">
          <p className="text-xs text-muted-foreground">{t('shell.userMenu.signedInAs')}</p>
          <p className="truncate text-sm font-semibold">{name}</p>
          {name !== user.username ? (
            <p className="truncate font-mono text-xs text-muted-foreground">{user.username}</p>
          ) : null}
          <p className="text-xs text-muted-foreground">
            {t(`shell.userMenu.source.${user.source}`)}
          </p>
        </div>
        <DropdownMenuLabel>{t('shell.userMenu.roles')}</DropdownMenuLabel>
        <p className="px-2 pb-1.5 text-sm">
          {roleNames.length === 0
            ? t('shell.userMenu.noRoles')
            : `${shown.join(', ')}${extra > 0 ? ` +${extra}` : ''}`}
        </p>
        <DropdownMenuSeparator />
        {ACCOUNT_ITEMS.map((item) => (
          <DropdownMenuItem key={item.id} asChild>
            <Link to={item.to}>
              <item.icon aria-hidden="true" />
              {t(item.labelKey)}
            </Link>
          </DropdownMenuItem>
        ))}
        <DropdownMenuSeparator />
        <DropdownMenuItem onSelect={signOut} disabled={logout.isPending}>
          <LogOut aria-hidden="true" />
          {logout.isPending ? t('shell.userMenu.signingOut') : t('shell.userMenu.signOut')}
        </DropdownMenuItem>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

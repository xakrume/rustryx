import { useQuery } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import {
  Boxes,
  LogOut,
  Monitor,
  Moon,
  Rows2,
  Rows3,
  Search,
  Sun,
  type LucideIcon,
} from 'lucide-react'
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from 'react'
import { api } from '@/api/client'
import { unwrap } from '@/api/request'
import {
  CommandDialog,
  CommandEmpty,
  CommandGroup,
  CommandInput,
  CommandItem,
  CommandList,
} from '@/components/ui/command'
import { satisfies } from '@/features/auth/permissions'
import { useSignOut } from '@/features/auth/api'
import { usePermissions } from '@/features/auth/use-permissions'
import { useTheme } from '@/features/theme/theme-provider'
import { useDebouncedValue } from '@/hooks/use-debounced-value'
import { t } from '@/i18n'
import { ACCOUNT_ITEMS, NAV_SECTIONS } from './nav-config'

export interface PaletteCommand {
  id: string
  label: string
  icon?: LucideIcon
  /** Extra words that make the command findable ("dark", "logout"). */
  keywords?: readonly string[]
  run: () => void
}

interface PaletteContextValue {
  open: boolean
  setOpen: (open: boolean) => void
  /**
   * Add commands for as long as the caller is mounted: `useEffect(() => register([...]), [])`.
   * The returned function removes them, which is what the effect cleanup needs.
   */
  register: (commands: readonly PaletteCommand[]) => () => void
}

const PaletteContext = createContext<PaletteContextValue | null>(null)

export function useCommandPalette(): PaletteContextValue {
  const ctx = useContext(PaletteContext)
  if (ctx === null)
    throw new Error('useCommandPalette must be used inside <CommandPaletteProvider>')
  return ctx
}

const REPOSITORY_LIMIT = 8

/**
 * Ctrl or Cmd plus K, from anywhere, opens a palette that lists only what the user may open:
 * pages (filtered by `/me`), repositories that match the typed text (the server filters them
 * by what the caller may browse), a component search, and a few commands. Feature modules
 * add their own commands through `register`.
 */
export function CommandPaletteProvider({ children }: { children: ReactNode }) {
  const [open, setOpen] = useState(false)
  const [registered, setRegistered] = useState<readonly (readonly PaletteCommand[])[]>([])

  const register = useCallback((commands: readonly PaletteCommand[]) => {
    setRegistered((current) => [...current, commands])
    return () => setRegistered((current) => current.filter((c) => c !== commands))
  }, [])

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent): void => {
      if ((event.ctrlKey || event.metaKey) && !event.altKey && event.key.toLowerCase() === 'k') {
        event.preventDefault()
        setOpen((current) => !current)
      }
    }
    document.addEventListener('keydown', onKeyDown)
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [])

  const value = useMemo(() => ({ open, setOpen, register }), [open, register])
  const extra = useMemo(() => registered.flat(), [registered])

  return (
    <PaletteContext.Provider value={value}>
      {children}
      <Palette open={open} onOpenChange={setOpen} extra={extra} />
    </PaletteContext.Provider>
  )
}

function Palette({
  open,
  onOpenChange,
  extra,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  extra: readonly PaletteCommand[]
}) {
  const [text, setText] = useState('')
  const navigate = useNavigate()
  const permissions = usePermissions()
  const { setPreference, setDensity } = useTheme()
  const { signOut } = useSignOut()
  const query = useDebouncedValue(text.trim(), 200)

  // Start empty each time the palette opens.
  useEffect(() => {
    if (!open) setText('')
  }, [open])

  const repositories = useQuery({
    queryKey: ['palette', 'repositories', query],
    queryFn: ({ signal }) =>
      unwrap(
        api.GET('/api/v1/repositories', {
          params: { query: { q: query, limit: REPOSITORY_LIMIT } },
          signal,
        }),
      ),
    enabled: open && query.length > 0,
    staleTime: 15_000,
    meta: { silent: true },
  })

  const pages = [...NAV_SECTIONS.flatMap((s) => s.items), ...ACCOUNT_ITEMS].filter(
    (item) => !item.need || satisfies(item.need, permissions),
  )

  function run(action: () => void): void {
    onOpenChange(false)
    action()
  }

  const actions: PaletteCommand[] = [
    {
      id: 'theme-light',
      label: `${t('shell.theme.label')}: ${t('shell.theme.light')}`,
      icon: Sun,
      keywords: ['theme', 'light'],
      run: () => setPreference('light'),
    },
    {
      id: 'theme-dark',
      label: `${t('shell.theme.label')}: ${t('shell.theme.dark')}`,
      icon: Moon,
      keywords: ['theme', 'dark'],
      run: () => setPreference('dark'),
    },
    {
      id: 'theme-system',
      label: `${t('shell.theme.label')}: ${t('shell.theme.system')}`,
      icon: Monitor,
      keywords: ['theme', 'system'],
      run: () => setPreference('system'),
    },
    {
      id: 'density-compact',
      label: `${t('shell.density.label')}: ${t('shell.density.compact')}`,
      icon: Rows3,
      keywords: ['density', 'compact'],
      run: () => setDensity('compact'),
    },
    {
      id: 'density-comfortable',
      label: `${t('shell.density.label')}: ${t('shell.density.comfortable')}`,
      icon: Rows2,
      keywords: ['density', 'comfortable'],
      run: () => setDensity('comfortable'),
    },
    ...(permissions.authenticated
      ? [
          {
            id: 'sign-out',
            label: t('shell.userMenu.signOut'),
            icon: LogOut,
            keywords: ['logout', 'sign out'],
            run: signOut,
          },
        ]
      : []),
    ...extra,
  ]

  return (
    <CommandDialog
      open={open}
      onOpenChange={onOpenChange}
      title={t('shell.palette.title')}
      description={t('shell.palette.description')}
      inputLabel={t('shell.palette.inputLabel')}
    >
      <CommandInput
        placeholder={t('shell.palette.inputPlaceholder')}
        value={text}
        onValueChange={setText}
      />
      <CommandList>
        <CommandEmpty />
        {query.length > 0 ? (
          <CommandGroup forceMount heading={t('shell.palette.groupSearch')}>
            <CommandItem
              forceMount
              value={`search-components ${query}`}
              onSelect={() => run(() => void navigate({ to: '/search', search: { q: query } }))}
            >
              <Search aria-hidden="true" />
              {t('shell.palette.searchComponents', { query })}
            </CommandItem>
          </CommandGroup>
        ) : null}
        <CommandGroup heading={t('shell.palette.groupPages')}>
          {pages.map((item) => (
            <CommandItem
              key={item.id}
              value={`page ${t(item.labelKey)}`}
              keywords={[item.id]}
              onSelect={() => run(() => void navigate({ to: item.to }))}
            >
              <item.icon aria-hidden="true" />
              {t(item.labelKey)}
            </CommandItem>
          ))}
        </CommandGroup>
        {repositories.data && repositories.data.items.length > 0 ? (
          <CommandGroup heading={t('shell.palette.groupRepositories')}>
            {repositories.data.items.map((repo) => (
              <CommandItem
                key={repo.id}
                value={`repository ${repo.name}`}
                onSelect={() =>
                  run(
                    () => void navigate({ to: '/repositories/$name', params: { name: repo.name } }),
                  )
                }
              >
                <Boxes aria-hidden="true" />
                <span className="font-mono">{repo.name}</span>
                <span className="ml-auto text-xs text-muted-foreground">
                  {repo.format} / {repo.repo_type}
                </span>
              </CommandItem>
            ))}
          </CommandGroup>
        ) : null}
        <CommandGroup heading={t('shell.palette.groupActions')}>
          {actions.map((command) => (
            <CommandItem
              key={command.id}
              value={`action ${command.label}`}
              keywords={command.keywords ? [...command.keywords] : undefined}
              onSelect={() => run(command.run)}
            >
              {command.icon ? <command.icon aria-hidden="true" /> : null}
              {command.label}
            </CommandItem>
          ))}
        </CommandGroup>
      </CommandList>
    </CommandDialog>
  )
}

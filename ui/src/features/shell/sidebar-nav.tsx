import { Link, useLocation } from '@tanstack/react-router'
import { ChevronDown, PanelLeftClose, PanelLeftOpen } from 'lucide-react'
import { useEffect, useState, type KeyboardEvent } from 'react'
import { Button } from '@/components/ui/button'
import { Tooltip, TooltipContent, TooltipTrigger } from '@/components/ui/tooltip'
import { satisfies } from '@/features/auth/permissions'
import { usePermissions } from '@/features/auth/use-permissions'
import { t } from '@/i18n'
import { cn } from '@/lib/utils'
import { NAV_SECTIONS, type NavItem, type NavSection } from './nav-config'

export interface SidebarNavProps {
  /** Icon rail: labels hidden (kept as accessible names and tooltips). */
  collapsed?: boolean
  /** Called after a link was followed, so the mobile drawer can close itself. */
  onNavigate?: () => void
  /** Present on desktop: renders the collapse toggle at the bottom. */
  onToggleCollapsed?: () => void
}

function NavLink({
  item,
  collapsed,
  onNavigate,
}: {
  item: NavItem
  collapsed: boolean
  onNavigate?: () => void
}) {
  const Icon = item.icon
  const label = t(item.labelKey)
  const link = (
    <Link
      to={item.to}
      activeOptions={{ exact: item.exact === true }}
      aria-label={collapsed ? label : undefined}
      onClick={onNavigate}
      className="group relative flex min-h-(--control-h) items-center gap-2 rounded-md px-3 text-sm font-medium text-foreground transition-colors duration-150 hover:bg-muted data-[status=active]:bg-secondary data-[status=active]:text-on-secondary data-[status=active]:before:absolute data-[status=active]:before:inset-y-1.5 data-[status=active]:before:left-0 data-[status=active]:before:w-1 data-[status=active]:before:rounded-full data-[status=active]:before:bg-accent"
      activeProps={{ 'aria-current': 'page' }}
    >
      <Icon className="size-4 shrink-0" aria-hidden="true" />
      {collapsed ? null : <span className="truncate">{label}</span>}
    </Link>
  )
  if (!collapsed) return <li>{link}</li>
  return (
    <li>
      <Tooltip>
        <TooltipTrigger asChild>{link}</TooltipTrigger>
        <TooltipContent side="right">{label}</TooltipContent>
      </Tooltip>
    </li>
  )
}

function Section({
  section,
  items,
  collapsed,
  onNavigate,
}: {
  section: NavSection
  items: readonly NavItem[]
  collapsed: boolean
  onNavigate?: () => void
}) {
  const pathname = useLocation({ select: (l) => l.pathname })
  const containsActive = items.some((item) =>
    item.exact ? pathname === item.to : pathname === item.to || pathname.startsWith(`${item.to}/`),
  )
  // Administration groups start collapsed unless the current page is inside them: the list
  // stays short on a laptop screen, and a group opens itself when the user navigates into it.
  const [open, setOpen] = useState(!section.collapsible || containsActive)
  useEffect(() => {
    if (containsActive) setOpen(true)
  }, [containsActive])
  const listId = `nav-${section.id}`
  const heading = section.labelKey ? t(section.labelKey) : null
  const showItems = collapsed || !section.collapsible || open
  return (
    <div className={cn(collapsed && heading && 'border-t border-border pt-2')}>
      {heading && !collapsed ? (
        section.collapsible ? (
          <h2 className="mb-0.5">
            <button
              type="button"
              aria-expanded={open}
              aria-controls={listId}
              onClick={() => setOpen((v) => !v)}
              className="flex min-h-8 w-full items-center justify-between rounded-md px-3 text-xs font-semibold tracking-wide text-muted-foreground uppercase hover:text-foreground"
            >
              <span>{heading}</span>
              <ChevronDown
                className={cn('size-4 transition-transform duration-150', !open && '-rotate-90')}
                aria-hidden="true"
              />
            </button>
          </h2>
        ) : (
          <h2 className="px-3 pb-1 text-xs font-semibold tracking-wide text-muted-foreground uppercase">
            {heading}
          </h2>
        )
      ) : null}
      {showItems ? (
        <ul id={listId} className="m-0 flex list-none flex-col gap-0.5 p-0">
          {items.map((item) => (
            <NavLink key={item.id} item={item} collapsed={collapsed} onNavigate={onNavigate} />
          ))}
        </ul>
      ) : null}
    </div>
  )
}

/**
 * Sidebar navigation: the sections of the page inventory, filtered by what `/me` says the
 * user may open (hidden, not disabled). Every item is a real link, reachable with Tab; Up,
 * Down, Home and End additionally move between items, so a long list does not cost a dozen
 * Tab presses. Collapsed to an icon rail the labels stay as accessible names and tooltips.
 */
export function SidebarNav({ collapsed = false, onNavigate, onToggleCollapsed }: SidebarNavProps) {
  const permissions = usePermissions()
  const sections = NAV_SECTIONS.map((section) => ({
    section,
    items: section.items.filter((item) => !item.need || satisfies(item.need, permissions)),
  })).filter((entry) => entry.items.length > 0)

  function onKeyDown(event: KeyboardEvent<HTMLElement>): void {
    if (!['ArrowDown', 'ArrowUp', 'Home', 'End'].includes(event.key)) return
    const focusables = Array.from(
      event.currentTarget.querySelectorAll<HTMLElement>('a[href], button'),
    )
    const index = focusables.indexOf(document.activeElement as HTMLElement)
    if (index < 0) return
    event.preventDefault()
    const next =
      event.key === 'Home'
        ? 0
        : event.key === 'End'
          ? focusables.length - 1
          : Math.max(
              0,
              Math.min(focusables.length - 1, index + (event.key === 'ArrowDown' ? 1 : -1)),
            )
    focusables[next]?.focus()
  }

  return (
    <div className="flex h-full flex-col">
      {/* The key handler only adds arrow navigation between the real links inside; every link is
          still reachable with Tab, so the landmark itself is not an interactive widget. */}
      {/* eslint-disable-next-line jsx-a11y/no-noninteractive-element-interactions -- see above */}
      <nav
        aria-label={t('a11y.mainNavigation')}
        onKeyDown={onKeyDown}
        className="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto p-2"
      >
        {sections.map(({ section, items }) => (
          <Section
            key={section.id}
            section={section}
            items={items}
            collapsed={collapsed}
            onNavigate={onNavigate}
          />
        ))}
      </nav>
      {onToggleCollapsed ? (
        <div className="border-t border-border p-2">
          <Button
            variant="ghost"
            className="w-full justify-start"
            size={collapsed ? 'icon' : 'default'}
            aria-label={collapsed ? t('nav.expand') : t('nav.collapse')}
            aria-expanded={!collapsed}
            onClick={onToggleCollapsed}
          >
            {collapsed ? (
              <PanelLeftOpen aria-hidden="true" />
            ) : (
              <PanelLeftClose aria-hidden="true" />
            )}
            {collapsed ? null : <span>{t('nav.collapse')}</span>}
          </Button>
        </div>
      ) : null}
    </div>
  )
}

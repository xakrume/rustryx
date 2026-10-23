import { Outlet, useLocation } from '@tanstack/react-router'
import { useEffect, useState } from 'react'
import { ProtectedRoute } from '@/features/auth/protected-route'
import { SessionWatcher } from '@/features/auth/session-watcher'
import { Banners } from './banners'
import { CommandPaletteProvider } from './command-palette'
import { SidebarNav } from './sidebar-nav'
import { TopBar } from './topbar'
import { Sheet, SheetContent, SheetDescription, SheetTitle } from '@/components/ui/sheet'
import { t } from '@/i18n'
import { useSidebarState } from './use-sidebar-state'

/**
 * Layout of every signed-in (or anonymous browsing) page: a persistent sidebar from 768px up
 * (collapsible to an icon rail), a drawer below that, a top bar, banners, and ONE scroll area
 * (<main>), so the page behind a dialog has nothing of its own to scroll.
 *
 * <main id="main-content" tabIndex={-1}> is the skip link target and the fallback of the focus
 * management after a route change (RouteFocusManager).
 */
export function AppShell() {
  const { collapsed, toggle } = useSidebarState()
  const [drawerOpen, setDrawerOpen] = useState(false)
  const pathname = useLocation({ select: (l) => l.pathname })

  // A link inside the drawer closes it, and so does any other navigation.
  useEffect(() => {
    setDrawerOpen(false)
  }, [pathname])

  return (
    <CommandPaletteProvider>
      <div className="flex h-dvh bg-background">
        <aside
          className="hidden shrink-0 border-r border-border bg-card transition-[width] duration-200 md:block"
          style={{ width: collapsed ? 'var(--sidebar-w-collapsed)' : 'var(--sidebar-w)' }}
        >
          <SidebarNav collapsed={collapsed} onToggleCollapsed={toggle} />
        </aside>
        <div className="flex min-w-0 flex-1 flex-col">
          <TopBar onOpenMenu={() => setDrawerOpen(true)} />
          <Banners />
          <main
            id="main-content"
            tabIndex={-1}
            className="min-h-0 flex-1 overflow-y-auto outline-none"
          >
            <div className="mx-auto w-full max-w-[100rem] p-4 md:p-6">
              <ProtectedRoute>
                <Outlet />
              </ProtectedRoute>
            </div>
          </main>
        </div>
      </div>
      <Sheet open={drawerOpen} onOpenChange={setDrawerOpen}>
        <SheetContent side="left" className="bg-card" hideCloseButton={false}>
          <SheetTitle className="border-b border-border px-4 py-3 text-base font-semibold">
            {t('common.appName')}
          </SheetTitle>
          <SheetDescription className="sr-only">{t('a11y.mainNavigation')}</SheetDescription>
          <div className="min-h-0 flex-1">
            <SidebarNav onNavigate={() => setDrawerOpen(false)} />
          </div>
        </SheetContent>
      </Sheet>
      <SessionWatcher />
    </CommandPaletteProvider>
  )
}

import { Outlet, useLocation } from '@tanstack/react-router'
import { useEffect, useState } from 'react'
import { Toaster } from '@/components/toaster'
import { Sheet, SheetContent, SheetDescription, SheetTitle } from '@/components/ui/sheet'
import { TooltipProvider } from '@/components/ui/tooltip'
import { ProtectedRoute } from '@/features/auth/protected-route'
import { SessionWatcher } from '@/features/auth/session-watcher'
import { t } from '@/i18n'
import { Banners } from './banners'
import { CommandPaletteProvider } from './command-palette'
import { SidebarNav } from './sidebar-nav'
import { TopBar } from './topbar'
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
    // Tooltip and toaster live here, not in the entry App: the login page does not need
    // them, and keeping Radix toast/tooltip out of the initial JS stays under the budget.
    <TooltipProvider delayDuration={300}>
      <CommandPaletteProvider>
        <div className="flex h-dvh bg-background">
          <aside
            className="hidden shrink-0 border-r border-border/70 bg-background transition-[width] duration-300 ease-standard md:block"
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
              <div className="mx-auto w-full max-w-[90rem] px-4 py-6 md:px-8 md:py-8 xl:px-12">
                <ProtectedRoute>
                  <Outlet />
                </ProtectedRoute>
              </div>
            </main>
          </div>
        </div>
        <Sheet open={drawerOpen} onOpenChange={setDrawerOpen}>
          <SheetContent side="left" className="bg-background" hideCloseButton={false}>
            <SheetTitle className="px-5 pt-5 pb-2 text-lg font-semibold tracking-[-0.02em]">
              {t('common.appName')}
            </SheetTitle>
            <SheetDescription className="sr-only">{t('a11y.mainNavigation')}</SheetDescription>
            <div className="min-h-0 flex-1">
              <SidebarNav onNavigate={() => setDrawerOpen(false)} />
            </div>
          </SheetContent>
        </Sheet>
        <SessionWatcher />
        <Toaster />
      </CommandPaletteProvider>
    </TooltipProvider>
  )
}

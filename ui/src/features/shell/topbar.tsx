import { Menu, Search } from 'lucide-react'
import { DensityToggle } from '@/components/density-toggle'
import { Button } from '@/components/ui/button'
import { Kbd } from '@/components/ui/kbd'
import { t } from '@/i18n'
import { Breadcrumbs } from './breadcrumbs'
import { useCommandPalette } from './command-palette'
import { ThemeMenu } from './theme-menu'
import { UserMenu } from './user-menu'

function shortcutLabel(): string {
  // Mac users press Cmd, everyone else Ctrl. Both work everywhere; this only labels the hint.
  const mac = typeof navigator !== 'undefined' && /mac|iphone|ipad/i.test(navigator.platform)
  return mac ? '⌘ K' : t('shell.search.shortcutHint')
}

export function TopBar({ onOpenMenu }: { onOpenMenu: () => void }) {
  const palette = useCommandPalette()
  return (
    <header className="relative z-20 flex min-h-(--topbar-h) items-center gap-1 border-b border-border/70 bg-background/80 px-2 backdrop-blur-xl backdrop-saturate-150 md:px-5">
      <Button
        variant="ghost"
        size="icon"
        className="md:hidden"
        aria-label={t('nav.openMenu')}
        onClick={onOpenMenu}
      >
        <Menu aria-hidden="true" />
      </Button>
      <div className="min-w-0 flex-1">
        <Breadcrumbs />
      </div>
      <Button
        variant="outline"
        className="mr-1 hidden w-64 justify-start gap-2 border-transparent bg-secondary/70 font-normal text-muted-foreground hover:bg-secondary lg:inline-flex dark:hover:bg-secondary"
        aria-label={t('shell.search.open')}
        aria-keyshortcuts="Control+K Meta+K"
        onClick={() => palette.setOpen(true)}
      >
        <Search aria-hidden="true" />
        <span className="flex-1 text-left">{t('shell.search.placeholder')}</span>
        <Kbd>{shortcutLabel()}</Kbd>
      </Button>
      <Button
        variant="ghost"
        size="icon"
        className="lg:hidden"
        aria-label={t('shell.search.open')}
        aria-keyshortcuts="Control+K Meta+K"
        onClick={() => palette.setOpen(true)}
      >
        <Search aria-hidden="true" />
      </Button>
      <ThemeMenu />
      <DensityToggle />
      <UserMenu />
    </header>
  )
}

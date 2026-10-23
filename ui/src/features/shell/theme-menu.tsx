import { Monitor, Moon, Sun } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { useTheme } from '@/features/theme/theme-provider'
import { t } from '@/i18n'
import { THEME_PREFERENCES, type ThemePreference } from '@/lib/constants'

const ICONS = { light: Sun, dark: Moon, system: Monitor } as const

/** Light, dark or follow the system. The trigger shows the RESOLVED theme's icon. */
export function ThemeMenu() {
  const { preference, resolved, setPreference } = useTheme()
  const Icon = ICONS[resolved]
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          aria-label={t('shell.theme.current', { value: t(`shell.theme.${preference}`) })}
        >
          <Icon aria-hidden="true" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent>
        <DropdownMenuLabel>{t('shell.theme.label')}</DropdownMenuLabel>
        <DropdownMenuRadioGroup
          value={preference}
          onValueChange={(value) => setPreference(value as ThemePreference)}
        >
          {THEME_PREFERENCES.map((p) => (
            <DropdownMenuRadioItem key={p} value={p}>
              {t(`shell.theme.${p}`)}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

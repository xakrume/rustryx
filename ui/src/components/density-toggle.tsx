import { Rows2, Rows3 } from 'lucide-react'
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
import { DENSITIES, type Density } from '@/lib/constants'

/**
 * Compact (default) or comfortable rows and controls. The choice is global: it sets
 * data-density on <html>, which re-points --control-h and --row-h (styles/tokens.css), so
 * every table and form follows. The 44px touch minimum applies on top, whatever is chosen.
 */
export function DensityToggle() {
  const { density, setDensity } = useTheme()
  const Icon = density === 'compact' ? Rows3 : Rows2
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button
          variant="ghost"
          size="icon"
          aria-label={t('shell.density.current', { value: t(`shell.density.${density}`) })}
        >
          <Icon aria-hidden="true" />
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent>
        <DropdownMenuLabel>{t('shell.density.label')}</DropdownMenuLabel>
        <DropdownMenuRadioGroup value={density} onValueChange={(v) => setDensity(v as Density)}>
          {DENSITIES.map((d) => (
            <DropdownMenuRadioItem key={d} value={d}>
              {t(`shell.density.${d}`)}
            </DropdownMenuRadioItem>
          ))}
        </DropdownMenuRadioGroup>
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

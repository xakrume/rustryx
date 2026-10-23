import { Circle, CircleCheck, CircleHelp, CircleX, Info, TriangleAlert } from 'lucide-react'
import type { ComponentProps } from 'react'
import { Badge } from '@/components/ui/badge'
import { t } from '@/i18n'

export type StatusKind = 'ok' | 'warning' | 'error' | 'info' | 'neutral' | 'unknown'

const config = {
  ok: { icon: CircleCheck, variant: 'success', labelKey: 'status.ok' },
  warning: { icon: TriangleAlert, variant: 'warning', labelKey: 'status.warning' },
  error: { icon: CircleX, variant: 'destructive', labelKey: 'status.error' },
  info: { icon: Info, variant: 'info', labelKey: 'status.info' },
  neutral: { icon: Circle, variant: 'neutral', labelKey: 'status.neutral' },
  unknown: { icon: CircleHelp, variant: 'neutral', labelKey: 'status.unknown' },
} as const

export interface StatusBadgeProps extends Omit<ComponentProps<'span'>, 'children'> {
  status: StatusKind
  /** Visible text. Defaults to the status name; pass "Online", "Failed" and so on. */
  label?: string
}

/**
 * Icon plus text, always both. The color is a third, redundant cue: a status must read the same
 * in grayscale, for color blind users and in forced colors mode (WCAG 1.4.1).
 */
export function StatusBadge({ status, label, ...props }: StatusBadgeProps) {
  const { icon: Icon, variant, labelKey } = config[status]
  return (
    <Badge variant={variant} {...props}>
      <Icon aria-hidden="true" />
      <span>{label ?? t(labelKey)}</span>
    </Badge>
  )
}

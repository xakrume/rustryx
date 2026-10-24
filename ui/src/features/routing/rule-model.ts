import '@/lib/zod-setup'
import { z } from 'zod'
import { t } from '@/i18n'
import type { RoutingRuleInput, RoutingRuleView } from './api'

export interface RuleFormValues {
  name: string
  description: string
  mode: 'block' | 'allow'
  patterns: { value: string }[]
}

export const EMPTY_RULE: RuleFormValues = {
  name: '',
  description: '',
  mode: 'block',
  patterns: [{ value: '' }],
}

export const ruleSchema = z
  .object({
    name: z.string(),
    description: z.string(),
    mode: z.enum(['block', 'allow']),
    patterns: z.array(z.object({ value: z.string() })),
  })
  .superRefine((v, ctx) => {
    const name = v.name.trim()
    if (name === '')
      ctx.addIssue({ code: 'custom', path: ['name'], message: t('routing.errors.nameRequired') })
    else if (name.length > 128) {
      ctx.addIssue({ code: 'custom', path: ['name'], message: t('routing.errors.nameLength') })
    }
    if (v.patterns.length === 0) {
      ctx.addIssue({
        code: 'custom',
        path: ['patterns'],
        message: t('routing.patterns.atLeastOne'),
      })
    }
    v.patterns.forEach((pattern, index) => {
      if (pattern.value.trim() === '') {
        ctx.addIssue({
          code: 'custom',
          path: ['patterns', index, 'value'],
          message: t('routing.patterns.required'),
        })
      }
    })
  })

export function valuesFromRule(rule: RoutingRuleView): RuleFormValues {
  return {
    name: rule.name,
    description: rule.description,
    mode: rule.mode,
    patterns: rule.patterns.map((value) => ({ value })),
  }
}

export function toInput(v: RuleFormValues): RoutingRuleInput {
  return {
    name: v.name.trim(),
    description: v.description.trim(),
    mode: v.mode,
    patterns: v.patterns.map((p) => p.value.trim()),
  }
}

/** The non empty patterns of the form, which is what the linter and the tester can use. */
export function usablePatterns(v: Pick<RuleFormValues, 'patterns'>): string[] {
  return v.patterns.map((p) => p.value.trim()).filter((p) => p !== '')
}

import { t } from '@/i18n'

export type ScopeAction = 'browse' | 'read' | 'write' | 'delete'
export type ScopePreset = 'full' | 'read' | 'publish' | 'custom'

export interface ScopeRow {
  key: number
  repository: string
  actions: ScopeAction[]
}

export const SCOPE_ACTIONS: readonly ScopeAction[] = ['browse', 'read', 'write', 'delete']

/**
 * Write includes read and browse (`push implies pull`), so a scope that carries write always
 * carries those two. Enforced where the user picks the actions, so it is visible, and again here
 * so a scope that is sent can never be a write-only surprise.
 */
export function withImplied(actions: readonly ScopeAction[]): ScopeAction[] {
  const set = new Set(actions)
  if (set.has('write')) {
    set.add('browse')
    set.add('read')
  }
  if (set.has('read')) set.add('browse')
  return SCOPE_ACTIONS.filter((a) => set.has(a))
}

/** The scope strings of the API (`repo:<repository|*>:<action>`) for a preset or custom rows. */
export function scopesOf(preset: ScopePreset, rows: readonly ScopeRow[]): string[] {
  if (preset === 'full') return []
  if (preset === 'read') return ['repo:*:browse', 'repo:*:read']
  if (preset === 'publish') return ['repo:*:browse', 'repo:*:read', 'repo:*:write']
  return rows
    .filter((row) => row.repository !== '' && row.actions.length > 0)
    .flatMap((row) => withImplied(row.actions).map((a) => `repo:${row.repository}:${a}`))
}

/** One scope string as words: "read on all repositories". Unknown shapes are shown as they are. */
export function describeScope(scope: string): string {
  if (scope === '*') return t('tokens.scope.everything')
  const match = /^repo:([^:]+):(.+)$/.exec(scope)
  if (match === null) return scope
  const [, repository, action] = match
  const repositoryText = repository === '*' ? t('tokens.scope.allRepositories') : (repository ?? '')
  const actionText =
    action === '*'
      ? t('tokens.scope.everything')
      : t(`identity.action.${action as ScopeAction}`).toLowerCase()
  return t('tokens.scope.action', { action: actionText, repository: repositoryText })
}

/** A short cell text for a list of scopes: the first one, and how many more. */
export function scopeSummary(scopes: readonly string[]): string {
  if (scopes.length === 0) return t('tokens.scope.full')
  const first = describeScope(scopes[0] ?? '')
  return scopes.length === 1 ? first : `${first} +${scopes.length - 1}`
}

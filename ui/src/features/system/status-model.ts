import type { StatusCheck } from './api'
import type { MessageKey } from '@/i18n'

/**
 * The dependencies the page reports as health, in display order, by the check id the server
 * uses (docs/api-conventions.md, `StatusCheck.id`). `redis` is not in the contract's list of
 * ids yet; a server that does not send it simply shows "not reported".
 */
export const DEPENDENCIES: readonly { id: string; label: MessageKey }[] = [
  { id: 'database', label: 'systemStatus.dependencies.postgres' },
  { id: 'redis', label: 'systemStatus.dependencies.redis' },
  { id: 'blob_store_health', label: 'systemStatus.dependencies.blobStores' },
  { id: 'oidc_reachable', label: 'systemStatus.dependencies.keycloak' },
  { id: 'ldap_reachable', label: 'systemStatus.dependencies.ldap' },
]

const DEPENDENCY_IDS = new Set(DEPENDENCIES.map((d) => d.id))

const RANK: Record<string, number> = { error: 0, warning: 1, unknown: 2, ok: 3 }

/** Checks that are not a dependency probe, worst first: what needs attention is at the top. */
export function attentionChecks(checks: readonly StatusCheck[]): StatusCheck[] {
  return checks
    .filter((c) => !DEPENDENCY_IDS.has(c.id))
    .sort((a, b) => (RANK[a.status] ?? 2) - (RANK[b.status] ?? 2))
}

export function checkById(checks: readonly StatusCheck[], id: string): StatusCheck | undefined {
  return checks.find((c) => c.id === id)
}

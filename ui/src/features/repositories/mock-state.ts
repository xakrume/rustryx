import type { components } from '@/api/schema'
import { buildRepositories, type MockUser } from '@/mocks/data'
import { readSession, userOf } from '@/mocks/session'
import { minutesAgo, repositories, visibleRepositories } from '@/mocks/store'

export type RepositoryView = components['schemas']['RepositoryView']
type RepositoryPermissions = components['schemas']['RepositoryPermissions']

/**
 * Mutable state of the repository mock. The mock is a fixture, so unlike application code it
 * edits its store in place: `repositories` (src/mocks/store.ts) is the one list the dashboard,
 * the list, the detail, the browse tree and the upload all read, so a repository created here
 * shows up everywhere. A page reload starts from the seed again; tests call `resetRepositories`.
 */
const versions = new Map<string, number>()

export function resetRepositories(): void {
  repositories.splice(0, repositories.length, ...buildRepositories())
  versions.clear()
}

export function sessionUser(): MockUser | null {
  return userOf(readSession())
}

/** Phase 1 rules: an administrator may do everything, a developer reads and writes hosted content. */
export function permissionsFor(repo: RepositoryView, user: MockUser | null): RepositoryPermissions {
  if (user === null) {
    return { browse: true, read: true, write: false, delete: false, admin: false }
  }
  const content = repo.repo_type !== 'group'
  return {
    browse: true,
    read: true,
    write: repo.repo_type === 'hosted' && (user.admin || user.username === 'alice'),
    delete: user.admin && content,
    admin: user.admin,
  }
}

export function present(repo: RepositoryView, withStats = true): RepositoryView {
  return {
    ...repo,
    // Statistics are "a few minutes old" by contract: the fixed seed instant would read as the future.
    stats: withStats && repo.stats ? { ...repo.stats, as_of: minutesAgo(4) } : null,
    permissions: permissionsFor(repo, sessionUser()),
  }
}

export function findVisible(name: string): RepositoryView | undefined {
  return visibleRepositories().find((r) => r.name === name)
}

export function versionOf(repo: RepositoryView): number {
  return versions.get(repo.id) ?? 1
}

export function bumpVersion(repo: RepositoryView): void {
  versions.set(repo.id, versionOf(repo) + 1)
}

export const etagOf = (repo: RepositoryView): string => `"repo-${repo.id}-v${versionOf(repo)}"`

const BLOB_STORES: Readonly<Record<string, { name: string; kind: 'local' | 's3' }>> = {
  '0192f6c4-5a1e-7000-8000-000000000029': { name: 'default', kind: 'local' },
  '0192f6c4-5a1e-7000-8000-00000000002a': { name: 'archive-s3', kind: 's3' },
}

export function blobStoreRef(id: string): RepositoryView['blob_store'] {
  const known = BLOB_STORES[id]
  return { id, name: known?.name ?? `store-${id.slice(-4)}`, kind: known?.kind ?? 'local' }
}

export const CLEANUP_POLICY_NAMES: Readonly<Record<string, string>> = {
  '0192f6c4-5a1e-7000-8000-00000000006f': 'maven-old-snapshots',
}

export const ROUTING_RULE_NAMES: Readonly<Record<string, string>> = {
  '0192f6c4-5a1e-7000-8000-000000000079': 'block-snapshots',
}

export function replaceRepository(next: RepositoryView): void {
  const index = repositories.findIndex((r) => r.id === next.id)
  if (index >= 0) repositories.splice(index, 1, next)
}

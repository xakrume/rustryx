import type { components } from '@/api/schema'

export type MeView = components['schemas']['MeView']
export type AdminArea = components['schemas']['AdminArea']
export type RepoAction = components['schemas']['Action']

type Can = MeView['permissions']['can']

/** What a page, a nav entry or a button needs. */
export type PermissionNeed =
  | 'authenticated'
  | 'admin'
  | { area: AdminArea }
  | { can: keyof Can }
  | { repository: string; action: RepoAction }
  | ((permissions: Permissions) => boolean)

export interface Permissions {
  authenticated: boolean
  isAdmin: boolean
  can: Can
  /** True for an administrator or when the area is granted (phase 1: administrators only). */
  hasArea: (area: AdminArea) => boolean
  /** True when a grant covers the action on the repository. Administrators hold every one. */
  canOnRepository: (repository: string, action: RepoAction) => boolean
}

const NOBODY: Can = {
  create_repository: false,
  upload: false,
  create_personal_token: false,
  view_audit: false,
  administer: false,
}

/**
 * Reduce the `/me` permission summary to questions the UI asks. This only decides what to
 * SHOW: the server authorizes every request on its own, so a wrong answer here costs a
 * confusing button, never a security hole (docs/api-conventions.md, section 12).
 */
export function derivePermissions(me: MeView | undefined): Permissions {
  const summary = me?.permissions
  const isAdmin = summary?.is_admin ?? false
  const areas = new Set<AdminArea>(summary?.admin_areas ?? [])
  const grants = summary?.repository_grants ?? []
  return {
    authenticated: me?.authenticated ?? false,
    isAdmin,
    can: summary?.can ?? NOBODY,
    hasArea: (area) => isAdmin || areas.has(area),
    canOnRepository: (repository, action) =>
      isAdmin ||
      grants.some(
        (g) => (g.repository === '*' || g.repository === repository) && g.actions.includes(action),
      ),
  }
}

export function satisfies(need: PermissionNeed, permissions: Permissions): boolean {
  if (typeof need === 'function') return need(permissions)
  if (need === 'authenticated') return permissions.authenticated
  if (need === 'admin') return permissions.isAdmin
  if ('area' in need) return permissions.hasArea(need.area)
  if ('can' in need) return permissions.can[need.can]
  return permissions.canOnRepository(need.repository, need.action)
}

/** The privilege name shown on the "You do not have access" page. */
export function describeNeed(need: PermissionNeed): string {
  if (typeof need === 'function') return 'a privilege your account does not have'
  if (need === 'authenticated') return 'a signed-in account'
  if (need === 'admin') return 'nx-admin'
  if ('area' in need) return `app.${need.area}.read (phase 1: nx-admin)`
  if ('can' in need) return need.can.replaceAll('_', ' ')
  return `repo.${need.action}(${need.repository})`
}

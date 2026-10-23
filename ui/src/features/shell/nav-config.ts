import {
  Boxes,
  Database,
  FileClock,
  Filter,
  Fingerprint,
  GitMerge,
  HardDrive,
  Home,
  Info,
  KeyRound,
  ListChecks,
  Route as RouteIcon,
  Search,
  Settings,
  ShieldCheck,
  Upload,
  UserCog,
  Users,
  Activity,
  Globe,
  type LucideIcon,
} from 'lucide-react'
import type { LinkProps } from '@tanstack/react-router'
import type { PermissionNeed } from '@/features/auth/permissions'
import type { MessageKey } from '@/i18n'

export interface NavItem {
  id: string
  labelKey: MessageKey
  to: LinkProps['to']
  icon: LucideIcon
  /** Hidden (not disabled) when unmet. The route repeats it in staticData.access. */
  need?: PermissionNeed
  /** Match only this exact path (Home), not everything below it. */
  exact?: boolean
}

export interface NavSection {
  id: string
  /** Null for the top group, which has no heading. */
  labelKey: MessageKey | null
  /** Collapsible sections are the administration groups. */
  collapsible: boolean
  items: readonly NavItem[]
}

/**
 * The navigation tree of docs/ui-page-inventory.md section 6, milestone M1 entries. A page
 * agent adds its entry here when the page leaves the placeholder state (M2 and M3 entries
 * are added by their own waves). Visibility follows the `/me` permission summary; the server
 * stays the authority.
 */
export const NAV_SECTIONS: readonly NavSection[] = [
  {
    id: 'browse',
    labelKey: null,
    collapsible: false,
    items: [
      { id: 'home', labelKey: 'nav.items.home', to: '/', icon: Home, exact: true },
      { id: 'repositories', labelKey: 'nav.items.repositories', to: '/repositories', icon: Boxes },
      { id: 'search', labelKey: 'nav.items.search', to: '/search', icon: Search },
      {
        id: 'upload',
        labelKey: 'nav.items.upload',
        to: '/upload',
        icon: Upload,
        need: { can: 'upload' },
      },
    ],
  },
  {
    id: 'content',
    labelKey: 'nav.sections.content',
    collapsible: true,
    items: [
      {
        id: 'cleanup-policies',
        labelKey: 'nav.items.cleanupPolicies',
        to: '/admin/cleanup-policies',
        icon: ListChecks,
        need: 'admin',
      },
      {
        id: 'routing-rules',
        labelKey: 'nav.items.routingRules',
        to: '/admin/routing-rules',
        icon: RouteIcon,
        need: 'admin',
      },
      {
        id: 'content-selectors',
        labelKey: 'nav.items.contentSelectors',
        to: '/admin/content-selectors',
        icon: Filter,
        need: { area: 'selectors' },
      },
    ],
  },
  {
    id: 'storage',
    labelKey: 'nav.sections.storage',
    collapsible: true,
    items: [
      {
        id: 'blob-stores',
        labelKey: 'nav.items.blobStores',
        to: '/admin/blob-stores',
        icon: HardDrive,
        need: { area: 'blobstores' },
      },
    ],
  },
  {
    id: 'identity',
    labelKey: 'nav.sections.identity',
    collapsible: true,
    items: [
      {
        id: 'users',
        labelKey: 'nav.items.users',
        to: '/admin/users',
        icon: Users,
        need: { area: 'users' },
      },
      {
        id: 'roles',
        labelKey: 'nav.items.roles',
        to: '/admin/roles',
        icon: UserCog,
        need: { area: 'roles' },
      },
      {
        id: 'privileges',
        labelKey: 'nav.items.privileges',
        to: '/admin/privileges',
        icon: ShieldCheck,
        need: { area: 'privileges' },
      },
      {
        id: 'group-mappings',
        labelKey: 'nav.items.groupMappings',
        to: '/admin/group-mappings',
        icon: GitMerge,
        need: { area: 'mappings' },
      },
      {
        id: 'auth-providers',
        labelKey: 'nav.items.authProviders',
        to: '/admin/auth/providers',
        icon: Fingerprint,
        need: 'admin',
      },
      {
        id: 'anonymous',
        labelKey: 'nav.items.anonymousAccess',
        to: '/admin/anonymous',
        icon: Globe,
        need: 'admin',
      },
      {
        id: 'api-tokens',
        labelKey: 'nav.items.apiTokens',
        to: '/admin/tokens',
        icon: KeyRound,
        need: { area: 'tokens' },
      },
    ],
  },
  {
    id: 'automation',
    labelKey: 'nav.sections.automation',
    collapsible: true,
    items: [
      {
        id: 'tasks',
        labelKey: 'nav.items.tasks',
        to: '/admin/tasks',
        icon: Database,
        need: { area: 'tasks' },
      },
    ],
  },
  {
    id: 'system',
    labelKey: 'nav.sections.system',
    collapsible: true,
    items: [
      {
        id: 'settings',
        labelKey: 'nav.items.settings',
        to: '/admin/settings',
        icon: Settings,
        need: { area: 'settings' },
      },
      {
        id: 'audit',
        labelKey: 'nav.items.auditLog',
        to: '/admin/audit',
        icon: FileClock,
        need: { area: 'audit' },
      },
      {
        id: 'status',
        labelKey: 'nav.items.status',
        to: '/admin/system/status',
        icon: Activity,
        need: { area: 'system' },
      },
    ],
  },
  {
    id: 'help',
    labelKey: null,
    collapsible: false,
    items: [{ id: 'about', labelKey: 'nav.items.about', to: '/about', icon: Info }],
  },
]

/** Account pages live in the user menu and the palette, not in the sidebar. */
export const ACCOUNT_ITEMS: readonly NavItem[] = [
  {
    id: 'profile',
    labelKey: 'nav.items.profile',
    to: '/me',
    icon: Fingerprint,
    need: 'authenticated',
    exact: true,
  },
  {
    id: 'tokens',
    labelKey: 'nav.items.personalTokens',
    to: '/me/tokens',
    icon: KeyRound,
    need: 'authenticated',
  },
  {
    id: 'preferences',
    labelKey: 'nav.items.preferences',
    to: '/me/preferences',
    icon: Settings,
    need: 'authenticated',
  },
]

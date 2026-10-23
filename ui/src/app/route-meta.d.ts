import type { MessageKey } from '@/i18n'
import type { PermissionNeed } from '@/features/auth/permissions'

declare module '@tanstack/react-router' {
  /** Declared once here so every route file can set them with type checking. */
  interface StaticDataRouteOption {
    /**
     * Breadcrumb label: a message key, or a function of the route params for dynamic names
     * (`({ params }) => params.name`). A route without `crumb` adds no breadcrumb.
     */
    crumb?: MessageKey | ((context: { params: Record<string, string> }) => string)
    /**
     * Who may open the page. Omitted or 'public' means anyone, anonymous visitors included;
     * the server still decides what they see. All matched routes must be satisfied, so a
     * layout route can protect a whole subtree. Enforced by <ProtectedRoute> in the shell.
     */
    access?: 'public' | PermissionNeed
  }
}

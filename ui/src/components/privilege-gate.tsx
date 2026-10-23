import type { ReactNode } from 'react'
import { satisfies, type PermissionNeed } from '@/features/auth/permissions'
import { usePermissions } from '@/features/auth/use-permissions'

export interface PrivilegeGateProps {
  need: PermissionNeed
  /** Rendered when the principal lacks the privilege. Nothing by default: hidden, not disabled. */
  fallback?: ReactNode
  /**
   * A node, or a function of `allowed` for "disabled with a reason" controls:
   * `{(allowed) => <Button disabled={!allowed}>Delete</Button>}` (the caller shows the reason).
   * With a function the fallback is not used: the function always renders.
   */
  children: ReactNode | ((allowed: boolean) => ReactNode)
}

/**
 * Shows UI only to principals whose `/me` summary allows it. This is navigation comfort, not
 * security: the server checks every request again and answers 403 or 404 on its own.
 */
export function PrivilegeGate({ need, fallback = null, children }: PrivilegeGateProps) {
  const allowed = satisfies(need, usePermissions())
  if (typeof children === 'function') return <>{children(allowed)}</>
  return <>{allowed ? children : fallback}</>
}

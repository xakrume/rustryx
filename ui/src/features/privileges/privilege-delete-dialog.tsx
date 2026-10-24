import { useState } from 'react'
import { isApiError, type Dependent } from '@/api/errors'
import { ConfirmDialog } from '@/components/confirm-dialog'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { useDeletePrivilege, type PrivilegeView } from './api'

/**
 * Deleting a privilege names it and lists the roles that include it. A privilege in use asks
 * to type the name and is deleted with `force=true`, which detaches it from those roles; one that
 * is not in use is deleted without force, so a 409 `resource_in_use` that appears anyway shows
 * its blockers and the next confirm forces.
 */
export function PrivilegeDeleteDialog({
  privilege,
  onClose,
}: {
  privilege: PrivilegeView | null
  onClose: () => void
}) {
  if (privilege === null) return null
  return <Content privilege={privilege} onClose={onClose} />
}

function Content({ privilege, onClose }: { privilege: PrivilegeView; onClose: () => void }) {
  const remove = useDeletePrivilege()
  const inUse = privilege.roles.length > 0
  const [force, setForce] = useState(inUse)
  const dependents: Dependent[] = privilege.roles.map((r) => ({
    kind: 'role',
    id: r.id,
    name: r.name,
  }))

  async function confirm(): Promise<void> {
    try {
      await remove.mutateAsync({ id: privilege.id, force })
      toast.success(t('privileges.delete.deleted', { name: privilege.name }))
    } catch (error) {
      if (isApiError(error) && error.code === 'resource_in_use') setForce(true)
      throw error
    }
  }

  return (
    <ConfirmDialog
      open
      onOpenChange={(open) => !open && onClose()}
      kind={t('privileges.delete.kind')}
      name={privilege.name}
      confirmLabel={t('privileges.delete.confirm')}
      requireTyping={inUse}
      description={
        inUse
          ? t('privileges.delete.inUse', { roles: privilege.roles.map((r) => r.name).join(', ') })
          : undefined
      }
      dependents={dependents}
      onConfirm={confirm}
    />
  )
}

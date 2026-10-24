import { useState } from 'react'
import { isApiError, type Dependent } from '@/api/errors'
import { ConfirmDialog } from '@/components/confirm-dialog'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import { useDeleteRole, type RoleView } from './api'

/**
 * Deleting a role names it, says what depends on it, and for a role in use asks to type the
 * name: the assignments of users and group mappings go with it (`force=true`). A role that is not
 * in use is deleted without force, so a 409 `resource_in_use` that appears anyway (someone
 * assigned it a second ago) is shown with its blockers, and the next confirm then forces.
 */
export function RoleDeleteDialog({
  role,
  onClose,
}: {
  role: RoleView | null
  onClose: () => void
}) {
  if (role === null) return null
  return <Content role={role} onClose={onClose} />
}

function Content({ role, onClose }: { role: RoleView; onClose: () => void }) {
  const remove = useDeleteRole()
  const inUse = role.assigned_users + role.mapped_groups > 0
  const [force, setForce] = useState(inUse)

  const users = t('roles.editor.users', { count: role.assigned_users })
  const groups = t('roles.editor.groups', { count: role.mapped_groups })
  const dependents: Dependent[] = [
    ...(role.assigned_users > 0
      ? [
          {
            kind: '',
            id: null,
            name: t('roles.delete.usersDependent', { count: role.assigned_users }),
          },
        ]
      : []),
    ...(role.mapped_groups > 0
      ? [
          {
            kind: '',
            id: null,
            name: t('roles.delete.groupsDependent', { count: role.mapped_groups }),
          },
        ]
      : []),
  ]

  async function confirm(): Promise<void> {
    try {
      await remove.mutateAsync({ id: role.id, force })
      toast.success(t('roles.delete.deleted', { name: role.name }))
    } catch (error) {
      if (isApiError(error) && error.code === 'resource_in_use') setForce(true)
      throw error
    }
  }

  return (
    <ConfirmDialog
      open
      onOpenChange={(open) => !open && onClose()}
      kind={t('roles.delete.kind')}
      name={role.name}
      confirmLabel={t('roles.delete.confirm')}
      requireTyping={inUse}
      description={inUse ? t('roles.delete.inUse', { users, groups }) : undefined}
      dependents={dependents}
      onConfirm={confirm}
    />
  )
}

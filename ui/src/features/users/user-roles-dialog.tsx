import { useQuery } from '@tanstack/react-query'
import { useState } from 'react'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'
import { useMe } from '@/features/auth/use-permissions'
import { rolePickerQuery } from '@/features/roles/api'
import { MultiPicker, type PickerItem } from '@/features/roles/multi-picker'
import { OriginBadge } from '@/features/roles/origin-badge'
import { t } from '@/i18n'
import { localAdminsQuery, type UserView } from './api'

const ADMIN = 'nx-admin'
const ANONYMOUS = 'nx-anonymous'

export interface UserRolesDialogProps {
  user: UserView
  pending: boolean
  onSave: (roleIds: string[]) => void
  onClose: () => void
}

/**
 * Edit the MANUAL roles of a user. Roles granted by groups are listed but cannot be removed
 * here (they follow the group). Two removals are blocked with an explanation instead of being
 * sent to fail: nx-admin of the last local administrator (409 `last_admin`) and nx-admin of the
 * account in use (409 `cannot_modify_self`). nx-admin is not offered to external accounts and
 * nx-anonymous never.
 */
export function UserRolesDialog({ user, pending, onSave, onClose }: UserRolesDialogProps) {
  const roles = useQuery(rolePickerQuery)
  const admins = useQuery(localAdminsQuery)
  const me = useMe()
  const manual = user.roles.filter((r) => r.origin === 'manual').map((r) => r.role.id)
  const [value, setValue] = useState<string[]>(manual)
  const external = user.source !== 'local'
  const adminRole = (roles.data ?? []).find((r) => r.name === ADMIN)
  const isAdminNow = adminRole !== undefined && manual.includes(adminRole.id)
  const removesAdmin = isAdminNow && adminRole !== undefined && !value.includes(adminRole.id)
  const onlyAdmin =
    admins.data !== undefined &&
    admins.data.length > 0 &&
    admins.data.every((a) => a.id === user.id)
  const selfDemote = removesAdmin && me?.user?.id === user.id
  const lastAdmin = removesAdmin && onlyAdmin
  const blocked = selfDemote || lastAdmin

  const items: PickerItem[] = (roles.data ?? []).map((r) => ({
    id: r.id,
    label: r.name,
    description: r.description || undefined,
    disabledReason:
      r.name === ANONYMOUS
        ? t('users.roles.anonymous')
        : r.name === ADMIN && external
          ? t('users.roles.adminExternal')
          : undefined,
  }))
  const mapped = user.roles.filter((r) => r.origin !== 'manual')
  const changed = value.length !== manual.length || value.some((id) => !manual.includes(id))

  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="max-w-2xl">
        <DialogHeader>
          <DialogTitle>{t('users.roles.editTitle', { name: user.username })}</DialogTitle>
          <DialogDescription>{t('users.roles.editDescription')}</DialogDescription>
        </DialogHeader>
        <div className="space-y-4">
          <MultiPicker
            legend={t('users.roles.picker')}
            what={t('users.roles.pickerWhat')}
            items={items}
            value={value}
            onChange={setValue}
          />
          {lastAdmin ? <Alert variant="warning">{t('users.roles.lastAdmin')}</Alert> : null}
          {selfDemote && !lastAdmin ? (
            <Alert variant="warning">{t('users.roles.selfDemote')}</Alert>
          ) : null}
          {mapped.length > 0 ? (
            <div className="space-y-1 text-sm">
              <p className="font-medium">{t('users.roles.mapped')}</p>
              <ul className="space-y-1">
                {mapped.map((r) => (
                  <li
                    key={`${r.role.id}-${r.origin}`}
                    className="flex flex-wrap items-center gap-2"
                  >
                    <span className="font-mono">{r.role.name}</span>
                    <OriginBadge origin={r.origin} />
                  </li>
                ))}
              </ul>
            </div>
          ) : null}
        </div>
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            {t('common.cancel')}
          </Button>
          <Button loading={pending} disabled={!changed || blocked} onClick={() => onSave(value)}>
            {t('users.roles.save')}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

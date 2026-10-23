import { useQuery } from '@tanstack/react-query'
import { useMemo } from 'react'
import { meQuery } from './api'
import { derivePermissions, type MeView, type Permissions } from './permissions'

/** The signed-in principal, or undefined while `/me` is still loading. */
export function useMe(): MeView | undefined {
  return useQuery(meQuery).data
}

export function usePermissions(): Permissions {
  const me = useMe()
  return useMemo(() => derivePermissions(me), [me])
}

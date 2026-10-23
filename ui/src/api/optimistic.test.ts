import { QueryClient } from '@tanstack/react-query'
import { describe, expect, it } from 'vitest'
import { applyOptimistic, optimisticMutation } from './optimistic'

interface Role {
  name: string
  enabled: boolean
}

describe('applyOptimistic', () => {
  it('replaces the cached value with a new object and can roll back', async () => {
    const client = new QueryClient()
    const original: Role = { name: 'a', enabled: false }
    client.setQueryData<Role>(['role'], original)

    const ctx = await applyOptimistic<Role>(client, ['role'], (role) => ({
      ...role,
      enabled: true,
    }))
    const patched = client.getQueryData<Role>(['role'])
    expect(patched).toEqual({ name: 'a', enabled: true })
    expect(patched).not.toBe(original)
    // The previous object was never mutated.
    expect(original.enabled).toBe(false)

    ctx.rollback()
    // Equal, not necessarily identical: the cache shares structure between equal values.
    expect(client.getQueryData<Role>(['role'])).toEqual(original)
  })

  it('does nothing when there is no cached value', async () => {
    const client = new QueryClient()
    const ctx = await applyOptimistic<Role>(client, ['missing'], (role) => ({
      ...role,
      enabled: true,
    }))
    expect(ctx.previous).toBeUndefined()
    expect(client.getQueryData(['missing'])).toBeUndefined()
  })
})

describe('optimisticMutation', () => {
  it('patches on mutate and restores on error', async () => {
    const client = new QueryClient()
    client.setQueryData<Role>(['role'], { name: 'a', enabled: false })
    const options = optimisticMutation<Role, boolean>({
      queryClient: client,
      queryKey: ['role'],
      updater: (role, enabled) => ({ ...role, enabled }),
    })
    const context = await options.onMutate(true)
    expect(client.getQueryData<Role>(['role'])?.enabled).toBe(true)
    options.onError(new Error('x'), true, context)
    expect(client.getQueryData<Role>(['role'])?.enabled).toBe(false)
  })
})

import type { QueryClient, QueryKey } from '@tanstack/react-query'

/**
 * Optimistic update helpers. The cache entry is replaced by a NEW object (updaters must not
 * mutate), and the previous value is kept so a failure puts the screen back exactly.
 */
export interface OptimisticContext<T> {
  previous: T | undefined
  rollback: () => void
}

/**
 * Cancel in-flight reads of `queryKey` (a late response would overwrite the optimistic value),
 * apply `updater` to the cached value, and return how to undo it. Does nothing when there is
 * no cached value to patch.
 */
export async function applyOptimistic<T>(
  queryClient: QueryClient,
  queryKey: QueryKey,
  updater: (current: T) => T,
): Promise<OptimisticContext<T>> {
  await queryClient.cancelQueries({ queryKey })
  const previous = queryClient.getQueryData<T>(queryKey)
  if (previous !== undefined) queryClient.setQueryData<T>(queryKey, updater(previous))
  return {
    previous,
    rollback: () => queryClient.setQueryData<T>(queryKey, previous),
  }
}

export interface OptimisticMutationConfig<T, TVars> {
  queryClient: QueryClient
  queryKey: QueryKey
  updater: (current: T, variables: TVars) => T
  /** Keys refetched once the mutation settles, success or failure. Defaults to `queryKey`. */
  invalidate?: readonly QueryKey[]
}

/**
 * Spread into `useMutation` to get optimistic update, rollback on error and a refetch on
 * settle, the three steps people forget one of:
 *
 *   useMutation({ mutationFn, ...optimisticMutation({ queryClient, queryKey, updater }) })
 */
export function optimisticMutation<T, TVars>(config: OptimisticMutationConfig<T, TVars>) {
  const keys = config.invalidate ?? [config.queryKey]
  return {
    onMutate: (variables: TVars) =>
      applyOptimistic<T>(config.queryClient, config.queryKey, (current) =>
        config.updater(current, variables),
      ),
    onError: (_error: unknown, _variables: TVars, context: OptimisticContext<T> | undefined) => {
      context?.rollback()
    },
    onSettled: () =>
      Promise.all(keys.map((queryKey) => config.queryClient.invalidateQueries({ queryKey }))),
  }
}

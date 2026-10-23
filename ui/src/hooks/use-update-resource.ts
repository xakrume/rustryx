import { useMutation, useQueryClient, type QueryKey } from '@tanstack/react-query'
import { useState } from 'react'
import { isApiError, type ApiError } from '@/api/errors'
import { applyOptimistic, type OptimisticContext } from '@/api/optimistic'
import type { Versioned } from '@/api/request'
import type { ConflictDialogProps } from '@/components/conflict-dialog'
import { copyText } from '@/components/copy-button'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'

export interface UpdateResourceConfig<TData, TVars> {
  /** Cache key of the `Versioned<TData>` the form was filled from. */
  queryKey: QueryKey
  /** Names the object in the conflict dialog: "role maven-publishers". */
  resourceName: string
  /** PUT or PATCH with `If-Match: etag`. Resolve with the new version (body and ETag). */
  update: (variables: TVars, etag: string | null) => Promise<Versioned<TData>>
  /** Re-read the current server version. Used by "Overwrite with my changes". */
  fetchCurrent: () => Promise<Versioned<TData>>
  /** Lists and details that change too; refetched after the update settles. */
  invalidate?: readonly QueryKey[]
  /** Optimistic patch of the cached object; rolled back on failure. Must not mutate. */
  optimistic?: (current: TData, variables: TVars) => TData
  /** Serializes the user's edit for "Copy my changes". Leave out to hide the button. */
  copyChanges?: (variables: TVars) => string
  onSuccess?: (data: TData) => void
  /** Any failure that is neither 412 nor 409 (those open the conflict dialog). Default: a toast. */
  onError?: (error: unknown) => void
}

interface ConflictState<TVars> {
  kind: 'stale' | 'state'
  error: ApiError
  variables: TVars
}

/**
 * Update a mutable object the way the contract demands (docs/api-conventions.md, section 8):
 * the ETag read with the object goes back in `If-Match`, and a lost update is resolved by the
 * user instead of silently overwriting or silently failing.
 *
 * - 412 `precondition_failed`: someone changed it since it was read. The conflict dialog
 *   offers reload, overwrite (re-read the fresh version, repeat the edit against it) and, when
 *   `copyChanges` is given, a copy of the user's edit to the clipboard.
 * - 409: the request conflicts with the state whatever the version (name taken, still in use).
 *   The dialog explains and lists `details.dependents`; there is nothing to reload.
 * - anything else: `onError`, by default an error toast with the code and request id.
 *
 * Render `<ConflictDialog {...conflict} />` next to the form. Updates never retry on their own.
 */
export function useUpdateResource<TData, TVars>(config: UpdateResourceConfig<TData, TVars>) {
  const queryClient = useQueryClient()
  const [conflict, setConflict] = useState<ConflictState<TVars> | null>(null)

  const mutation = useMutation<
    Versioned<TData>,
    unknown,
    TVars,
    OptimisticContext<Versioned<TData>> | undefined
  >({
    // Failures are handled here (dialog or onError), so the global error toast stays quiet.
    meta: { silent: true },
    mutationFn: (variables) => {
      const cached = queryClient.getQueryData<Versioned<TData>>(config.queryKey)
      return config.update(variables, cached?.etag ?? null)
    },
    onMutate: config.optimistic
      ? (variables) =>
          applyOptimistic<Versioned<TData>>(queryClient, config.queryKey, (current) => ({
            data: config.optimistic!(current.data, variables),
            etag: current.etag,
          }))
      : undefined,
    onSuccess: (result) => {
      queryClient.setQueryData<Versioned<TData>>(config.queryKey, result)
      setConflict(null)
      config.onSuccess?.(result.data)
    },
    onError: (error, variables, context) => {
      context?.rollback()
      if (isApiError(error) && (error.kind === 'precondition' || error.kind === 'conflict')) {
        setConflict({ kind: error.kind === 'precondition' ? 'stale' : 'state', error, variables })
        return
      }
      if (config.onError) config.onError(error)
      else toast.apiError(error, config.resourceName)
    },
    onSettled: () =>
      Promise.all(
        [config.queryKey, ...(config.invalidate ?? [])].map((queryKey) =>
          queryClient.invalidateQueries({ queryKey }),
        ),
      ),
  })

  const { mutate } = mutation

  const conflictProps: ConflictDialogProps = {
    open: conflict !== null,
    kind: conflict?.kind ?? 'stale',
    resourceName: config.resourceName,
    error: conflict?.error ?? null,
    pending: mutation.isPending,
    onClose: () => setConflict(null),
    onReload: () => {
      setConflict(null)
      void queryClient.invalidateQueries({ queryKey: config.queryKey })
    },
    onOverwrite:
      conflict?.kind === 'stale'
        ? () => {
            const variables = conflict.variables
            // The new ETag goes into the cache first: mutationFn reads it from there.
            void config.fetchCurrent().then(
              (fresh) => {
                queryClient.setQueryData<Versioned<TData>>(config.queryKey, fresh)
                mutate(variables)
              },
              (error: unknown) => toast.apiError(error, config.resourceName),
            )
          }
        : undefined,
    onCopyMine:
      conflict?.kind === 'stale' && config.copyChanges
        ? () => {
            const text = config.copyChanges!(conflict.variables)
            void copyText(text).then((ok) => {
              if (ok) toast.success(t('errors.conflictDialog.copiedMine'))
              else toast.error(t('code.copyFailed'))
            })
          }
        : undefined,
  }

  return {
    mutate,
    mutateAsync: mutation.mutateAsync,
    isPending: mutation.isPending,
    error: mutation.error,
    /** Spread into <ConflictDialog />. */
    conflict: conflictProps,
  }
}

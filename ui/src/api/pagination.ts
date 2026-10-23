import {
  infiniteQueryOptions,
  useInfiniteQuery,
  type QueryKey,
  type UseInfiniteQueryResult,
  type InfiniteData,
} from '@tanstack/react-query'
import { useEffect, useMemo } from 'react'

/** The list envelope of the API: `{ items, next }`, `next` is null on the last page. */
export interface Page<T> {
  items: readonly T[]
  next: string | null
}

export interface PageRequest {
  /** Opaque token from the previous page, null for the first page. Never parse or build one. */
  after: string | null
  signal: AbortSignal
}

export interface InfinitePageConfig<T> {
  /**
   * Must contain every filter and the sort order: the `after` token is only valid with the
   * same endpoint, filters and sort, so changing any of them has to start a new query
   * (docs/api-conventions.md, section 6).
   */
  queryKey: QueryKey
  fetchPage: (request: PageRequest) => Promise<Page<T>>
  enabled?: boolean
  staleTime?: number
}

/**
 * A page can hold fewer items than `limit`, even none, while `next` is not null: items the
 * caller may not see are dropped after the fetch. Only `next: null` ends a list. After this
 * many consecutive empty pages the hook stops chasing and shows "load more" instead, so a
 * heavily filtered list cannot loop forever.
 */
const MAX_EMPTY_PAGES_IN_A_ROW = 10

export function infinitePageOptions<T>(config: InfinitePageConfig<T>) {
  return infiniteQueryOptions({
    queryKey: config.queryKey,
    initialPageParam: null as string | null,
    queryFn: ({ pageParam, signal }) => config.fetchPage({ after: pageParam, signal }),
    getNextPageParam: (last: Page<T>) => last.next ?? undefined,
    enabled: config.enabled,
    staleTime: config.staleTime,
  })
}

export interface InfinitePageResult<T> {
  items: readonly T[]
  isLoading: boolean
  isError: boolean
  error: unknown
  isFetching: boolean
  hasNextPage: boolean
  isLoadingMore: boolean
  loadMore: () => void
  refetch: () => void
  query: UseInfiniteQueryResult<InfiniteData<Page<T>>, Error>
}

/** Keyset pagination for tables and lists: flat `items`, `loadMore`, no total (there is none). */
export function useInfinitePage<T>(config: InfinitePageConfig<T>): InfinitePageResult<T> {
  const query = useInfiniteQuery(infinitePageOptions(config))
  const pages = query.data?.pages
  const items = useMemo(() => pages?.flatMap((p) => p.items) ?? [], [pages])

  let trailingEmpty = 0
  for (let i = (pages?.length ?? 0) - 1; i >= 0 && pages?.[i]?.items.length === 0; i -= 1) {
    trailingEmpty += 1
  }
  const lastNext = pages?.at(-1)?.next ?? null
  const chase =
    pages !== undefined &&
    trailingEmpty > 0 &&
    lastNext !== null &&
    trailingEmpty < MAX_EMPTY_PAGES_IN_A_ROW

  const { fetchNextPage, isFetching } = query
  const pageCount = pages?.length ?? 0
  // `pageCount` is a dependency on purpose: a page that arrives instantly never shows an
  // `isFetching` flip to React (the renders are batched), so the arrival of a page itself has
  // to re-run the check or the chase would stop after the second request.
  useEffect(() => {
    if (chase && !isFetching) void fetchNextPage()
  }, [chase, isFetching, fetchNextPage, pageCount])

  return {
    items,
    isLoading: query.isPending,
    isError: query.isError,
    error: query.error,
    isFetching,
    hasNextPage: query.hasNextPage,
    isLoadingMore: query.isFetchingNextPage,
    loadMore: () => void fetchNextPage(),
    refetch: () => void query.refetch(),
    query,
  }
}

import { QueryClientProvider } from '@tanstack/react-query'
import { act, renderHook, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { describe, expect, it, vi } from 'vitest'
import { createTestQueryClient } from '@/test/utils'
import { useInfinitePage, type Page, type PageRequest } from './pagination'

/** One client per test: a wrapper that built a new client per render would reset the cache on every render. */
function makeWrapper() {
  const client = createTestQueryClient()
  return function Wrapper({ children }: { children: ReactNode }) {
    return <QueryClientProvider client={client}>{children}</QueryClientProvider>
  }
}

const page = (items: number[], next: string | null): Page<number> => ({ items, next })

describe('useInfinitePage', () => {
  it('flattens pages and follows the opaque token with loadMore', async () => {
    const fetchPage = vi.fn(({ after }: PageRequest) =>
      Promise.resolve(after === null ? page([1, 2], 'tok-1') : page([3], null)),
    )
    const { result } = renderHook(() => useInfinitePage({ queryKey: ['list'], fetchPage }), {
      wrapper: makeWrapper(),
    })

    await waitFor(() => expect(result.current.items).toEqual([1, 2]))
    expect(result.current.hasNextPage).toBe(true)

    act(() => result.current.loadMore())
    await waitFor(() => expect(result.current.items).toEqual([1, 2, 3]))
    expect(result.current.hasNextPage).toBe(false)
    expect(fetchPage.mock.calls.map(([r]) => r.after)).toEqual([null, 'tok-1'])
  })

  it('keeps fetching while a page is empty but next is not null (filtered out items)', async () => {
    const fetchPage = vi.fn(({ after }: PageRequest) => {
      if (after === null) return Promise.resolve(page([], 'a'))
      if (after === 'a') return Promise.resolve(page([], 'b'))
      return Promise.resolve(page([7], null))
    })
    const { result } = renderHook(() => useInfinitePage({ queryKey: ['filtered'], fetchPage }), {
      wrapper: makeWrapper(),
    })
    await waitFor(() => expect(result.current.items).toEqual([7]))
    expect(fetchPage).toHaveBeenCalledTimes(3)
  })

  it('must_not_chase_empty_pages_forever', async () => {
    let calls = 0
    const fetchPage = vi.fn(() => {
      calls += 1
      return Promise.resolve(page([], `t${calls}`))
    })
    const { result } = renderHook(() => useInfinitePage({ queryKey: ['never'], fetchPage }), {
      wrapper: makeWrapper(),
    })
    await waitFor(() => expect(calls).toBeGreaterThanOrEqual(10))
    await new Promise((resolve) => setTimeout(resolve, 100))
    expect(calls).toBe(10)
    // The user can still ask for more by hand.
    expect(result.current.hasNextPage).toBe(true)
  })

  it('reports a failure of the first page', async () => {
    const fetchPage = vi.fn(() => Promise.reject(new Error('boom')))
    const { result } = renderHook(() => useInfinitePage({ queryKey: ['fail'], fetchPage }), {
      wrapper: makeWrapper(),
    })
    await waitFor(() => expect(result.current.isError).toBe(true))
    expect(result.current.items).toEqual([])
  })

  it('starts again from the first page when the key (filters or sort) changes', async () => {
    const fetchPage = vi.fn(({ after }: PageRequest) =>
      Promise.resolve(page([after === null ? 1 : 2], 'tok')),
    )
    const { result, rerender } = renderHook(
      ({ q }: { q: string }) => useInfinitePage({ queryKey: ['bound', q], fetchPage }),
      { wrapper: makeWrapper(), initialProps: { q: 'a' } },
    )
    await waitFor(() => expect(result.current.items).toEqual([1]))
    act(() => result.current.loadMore())
    await waitFor(() => expect(result.current.items).toEqual([1, 2]))
    rerender({ q: 'b' })
    await waitFor(() => expect(result.current.items).toEqual([1]))
    // The last call after the switch used no token: a cursor never crosses a filter change.
    expect(fetchPage.mock.calls.at(-1)?.[0].after).toBeNull()
  })
})

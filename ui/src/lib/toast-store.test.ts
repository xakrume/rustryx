import { describe, expect, it } from 'vitest'
import { ApiError } from '@/api/errors'
import { toast, toastStore } from './toast-store'

describe('toast store', () => {
  it('adds immutable snapshots and dismisses by id', () => {
    const before = toastStore.getSnapshot()
    const id = toast.success('Saved', { description: 'maven' })
    const after = toastStore.getSnapshot()
    expect(after).not.toBe(before)
    expect(after.at(-1)).toMatchObject({
      id,
      variant: 'success',
      title: 'Saved',
      description: 'maven',
    })
    toast.dismiss(id)
    expect(toastStore.getSnapshot().some((t) => t.id === id)).toBe(false)
  })

  it('keeps at most four toasts, dropping the oldest', () => {
    for (let i = 0; i < 6; i += 1) toast.info(`n${i}`)
    const titles = toastStore.getSnapshot().map((t) => t.title)
    expect(titles).toEqual(['n2', 'n3', 'n4', 'n5'])
  })

  it('shows the code and request id of an API failure, and keeps errors longer', () => {
    toast.apiError(
      new ApiError({
        code: 'quota_exceeded',
        message: 'Quota exceeded',
        status: 507,
        requestId: 'req-9',
      }),
      'Upload failed',
    )
    const item = toastStore.getSnapshot().at(-1)
    expect(item).toMatchObject({
      variant: 'error',
      title: 'Upload failed',
      code: 'quota_exceeded',
      requestId: 'req-9',
    })
    expect(item?.durationMs).toBeGreaterThan(8000)
  })

  it('must_not_leak_the_text_of_an_unexpected_error', () => {
    toast.apiError(new Error('SELECT * FROM users failed at /srv/app.rs:42'))
    const item = toastStore.getSnapshot().at(-1)
    expect(JSON.stringify(item)).not.toContain('SELECT')
    expect(item?.code).toBeUndefined()
  })
})

import { describe, expect, it } from 'vitest'
import { z } from './zod-setup'

describe('zod setup', () => {
  it('must_not_use_the_JIT_compiler (it needs unsafe-eval)', () => {
    expect(z.config().jitless).toBe(true)
  })

  it('still validates', () => {
    const schema = z.object({ name: z.string().min(1) })
    expect(schema.safeParse({ name: 'a' }).success).toBe(true)
    expect(schema.safeParse({ name: '' }).success).toBe(false)
  })
})

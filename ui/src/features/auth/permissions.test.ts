import { describe, expect, it } from 'vitest'
import { meBody } from '@/mocks/session'
import { describeNeed, derivePermissions, satisfies } from './permissions'

const session = (username: string) => ({
  username,
  csrf: 'c',
  issuedAt: Date.now(),
  touchedAt: Date.now(),
})

describe('derivePermissions', () => {
  it('knows nothing about a missing principal', () => {
    const p = derivePermissions(undefined)
    expect(p.authenticated).toBe(false)
    expect(p.isAdmin).toBe(false)
    expect(p.hasArea('users')).toBe(false)
    expect(p.can.upload).toBe(false)
  })

  it('gives an administrator every area and every repository action', () => {
    const p = derivePermissions(meBody(session('admin')))
    expect(p.isAdmin).toBe(true)
    expect(p.hasArea('audit')).toBe(true)
    expect(p.canOnRepository('anything', 'delete')).toBe(true)
  })

  it('limits a developer to the grants of the summary', () => {
    const p = derivePermissions(meBody(session('alice')))
    expect(p.isAdmin).toBe(false)
    expect(p.hasArea('users')).toBe(false)
    expect(p.canOnRepository('maven-releases', 'browse')).toBe(true)
    expect(p.canOnRepository('maven-releases', 'write')).toBe(false)
    expect(p.can.upload).toBe(true)
  })

  it('treats an anonymous principal as not authenticated', () => {
    const p = derivePermissions(meBody(null))
    expect(p.authenticated).toBe(false)
    expect(p.canOnRepository('x', 'browse')).toBe(true)
    expect(p.canOnRepository('x', 'write')).toBe(false)
  })
})

describe('satisfies and describeNeed', () => {
  const admin = derivePermissions(meBody(session('admin')))
  const alice = derivePermissions(meBody(session('alice')))
  const anon = derivePermissions(meBody(null))

  it('evaluates every kind of requirement', () => {
    expect(satisfies('authenticated', alice)).toBe(true)
    expect(satisfies('authenticated', anon)).toBe(false)
    expect(satisfies('admin', alice)).toBe(false)
    expect(satisfies('admin', admin)).toBe(true)
    expect(satisfies({ area: 'users' }, alice)).toBe(false)
    expect(satisfies({ can: 'upload' }, alice)).toBe(true)
    expect(satisfies({ repository: 'r', action: 'write' }, alice)).toBe(false)
    expect(satisfies((p) => p.authenticated && !p.isAdmin, alice)).toBe(true)
  })

  it('names the missing privilege for the forbidden page', () => {
    expect(describeNeed('admin')).toBe('nx-admin')
    expect(describeNeed({ area: 'users' })).toContain('app.users.read')
    expect(describeNeed({ repository: 'maven', action: 'write' })).toBe('repo.write(maven)')
    expect(describeNeed({ can: 'create_repository' })).toBe('create repository')
  })
})

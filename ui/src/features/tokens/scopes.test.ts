import { describe, expect, it } from 'vitest'
import { describeScope, scopeSummary, scopesOf, withImplied, type ScopeRow } from './scopes'

describe('token scopes', () => {
  it('write includes read and browse, read includes browse', () => {
    expect(withImplied(['write'])).toEqual(['browse', 'read', 'write'])
    expect(withImplied(['read'])).toEqual(['browse', 'read'])
    expect(withImplied(['delete'])).toEqual(['delete'])
    expect(withImplied([])).toEqual([])
  })

  it('builds the scope strings of a preset or of custom rows', () => {
    expect(scopesOf('full', [])).toEqual([])
    expect(scopesOf('read', [])).toEqual(['repo:*:browse', 'repo:*:read'])
    expect(scopesOf('publish', [])).toEqual(['repo:*:browse', 'repo:*:read', 'repo:*:write'])
    const rows: ScopeRow[] = [
      { key: 1, repository: 'maven-releases', actions: ['write'] },
      { key: 2, repository: '', actions: ['read'] },
      { key: 3, repository: '*', actions: [] },
    ]
    expect(scopesOf('custom', rows)).toEqual([
      'repo:maven-releases:browse',
      'repo:maven-releases:read',
      'repo:maven-releases:write',
    ])
  })

  it('describes scopes in words', () => {
    expect(describeScope('*')).toBe('Everything the owner can do')
    expect(describeScope('repo:*:read')).toBe('read on all repositories')
    expect(describeScope('repo:maven-releases:write')).toBe('write on maven-releases')
    expect(describeScope('repo:x:*')).toBe('Everything the owner can do on x')
    expect(describeScope('something else')).toBe('something else')
  })

  it('summarizes a list for a table cell', () => {
    expect(scopeSummary([])).toBe('Same as my access')
    expect(scopeSummary(['repo:*:read'])).toBe('read on all repositories')
    expect(scopeSummary(['repo:*:read', 'repo:*:browse'])).toBe('read on all repositories +1')
  })
})

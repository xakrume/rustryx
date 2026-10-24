import { describe, expect, it } from 'vitest'
import { fillPlaceholders } from './snippet-block'
import { buildSnippets, type SnippetInput } from './mock-snippets'

const base: SnippetInput = {
  format: 'maven2',
  name: 'repo',
  url: 'https://repo.example.org/repository/repo/',
  publishUrl: 'https://repo.example.org/repository/repo/',
  publishName: 'repo',
  anonymous: false,
  canWrite: true,
}

describe('snippets', () => {
  it('fills placeholders for the current view only and leaves unknown ones alone', () => {
    expect(fillPlaceholders('a <TOKEN> b <USERNAME> c <FILE>', { TOKEN: 't', USERNAME: 'u' })).toBe(
      'a t b u c <FILE>',
    )
  })

  it.each(['maven2', 'npm', 'pypi', 'docker', 'oci', 'helm', 'pub', 'raw'] as const)(
    'must_not_embed_a_credential_in_the_%s_snippets',
    (format) => {
      const snippets = buildSnippets({ ...base, format })
      expect(snippets.length).toBeGreaterThan(0)
      for (const snippet of snippets) {
        expect(snippet.content).not.toMatch(/ryx_[A-Za-z0-9]/)
        // Whatever needs a secret asks for it by a placeholder the panel lists.
        if (/<TOKEN>/.test(snippet.content)) {
          expect(snippet.placeholders.map((p) => p.name)).toContain('TOKEN')
        }
      }
    },
  )

  it('leaves the credentials out for an anonymous variant and the publish snippets out without write', () => {
    for (const format of ['maven2', 'npm', 'pypi', 'docker', 'helm', 'pub', 'raw'] as const) {
      const anonymous = buildSnippets({ ...base, format, anonymous: true, canWrite: false })
      expect(anonymous.some((s) => s.purpose === 'publish')).toBe(false)
      expect(anonymous.some((s) => s.purpose === 'login')).toBe(false)
    }
  })

  it('has no template for a format it does not know', () => {
    expect(buildSnippets({ ...base, format: 'unknown' as never })).toEqual([])
  })
})

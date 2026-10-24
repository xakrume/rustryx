import { describe, expect, it } from 'vitest'
import { compile, LIMITS, verdictOf, type Compiled } from './mock-engine'

const ok = (source: string): Compiled => {
  const result = compile(source)
  if (Array.isArray(result)) throw new Error(`does not compile: ${JSON.stringify(result)}`)
  return result
}
type Found = Extract<ReturnType<typeof compile>, unknown[]>[number]
const issues = (source: string): [Found, ...Found[]] => {
  const result = compile(source)
  if (!Array.isArray(result) || result.length === 0) throw new Error('compiles')
  return result as [Found, ...Found[]]
}
const sample = (path: string, extra: Record<string, unknown> = {}) => ({
  format: 'maven2' as const,
  path,
  repository: 'r',
  ...extra,
})

describe('selector engine stand-in', () => {
  it('compiles the documented subset', () => {
    ok('format == "maven2" && path.startsWith("/com/")')
    ok('!(path =~ ".*-SNAPSHOT.*") || format in ["npm", "pypi"]')
    ok('coordinate.name.contains("x") && repository != "y" && path.endsWith(".jar")')
    ok('format == "a\\"b"')
  })

  it('reports an empty and a too long expression', () => {
    expect(issues('  ')[0]).toMatchObject({ kind: 'empty', offset: null })
    const long = issues(`path == "${'a'.repeat(LIMITS.max_expression_bytes)}"`)[0]
    expect(long).toMatchObject({ kind: 'too_long', offset: LIMITS.max_expression_bytes })
  })

  it('reports syntax errors with the byte offset of the token', () => {
    expect(issues('format == ')[0]).toMatchObject({ kind: 'syntax', offset: 10 })
    expect(issues('format == "x" )')[0]).toMatchObject({ kind: 'syntax', offset: 14 })
    expect(issues('format = "x"')[0]).toMatchObject({ kind: 'syntax', offset: 7 })
    expect(issues('format == "x')[0].message).toContain('unterminated')
    expect(issues('(format == "x"')[0].message).toContain('ends')
    expect(issues('format in "x"')[0].message).toContain('unexpected token')
    expect(issues('format in [x]')[0].message).toContain('expected a string')
    expect(issues('path.startsWith(x)')[0].message).toContain('takes a string')
    // The offset counts bytes: the accent before the error is two bytes.
    expect(issues('format == "é" &&')[0].offset).toBe(17)
  })

  it('reports type errors', () => {
    expect(issues('nope == "x"')[0]).toMatchObject({ kind: 'type' })
    expect(issues('format')[0].message).toContain('true or false')
    expect(issues('format && path')[0].message).toContain('booleans')
    expect(issues('!format')[0].message).toContain('boolean')
    expect(issues('(format == "x") in ["a"]')[0].message).toContain('string and a list')
    expect(issues('"a" == ("b" == "c")')[0].message).toContain('compares strings')
  })

  it('reports an invalid and an oversized regex and a nesting that is too deep', () => {
    expect(issues('path =~ "("')[0]).toMatchObject({ kind: 'regex' })
    expect(issues(`path =~ "${'a'.repeat(LIMITS.max_regex_bytes + 1)}"`)[0].message).toContain(
      'longer',
    )
    const deep = `${'('.repeat(40)}format == "x"${')'.repeat(40)}`
    expect(issues(deep)[0].kind).toBe('too_deep')
    expect(issues(`path in [${Array.from({ length: 65 }, () => '"a"').join(',')}]`)[0].kind).toBe(
      'too_long',
    )
  })

  it('evaluates strings, lists and regexes exactly', () => {
    const maven = ok('format == "maven2" && path.startsWith("/com/example/")')
    expect(verdictOf(maven, sample('/com/example/a.jar'))).toEqual({
      result: 'match',
      reason: null,
    })
    expect(verdictOf(maven, sample('/org/a.jar')).result).toBe('no_match')
    expect(verdictOf(ok('format in ["npm", "maven2"]'), sample('/x')).result).toBe('match')
    expect(verdictOf(ok('format != "maven2"'), sample('/x')).result).toBe('no_match')
    expect(verdictOf(ok('path =~ ".*\\.jar"'), sample('/a.jar')).result).toBe('match')
    // The regex must match the WHOLE string.
    expect(verdictOf(ok('path =~ "a"'), sample('/a.jar')).result).toBe('no_match')
    expect(verdictOf(ok('path.endsWith(".jar")'), sample('/a.jar')).result).toBe('match')
    expect(verdictOf(ok('path.contains("com")'), sample('/com/x')).result).toBe('match')
    expect(
      verdictOf(ok('repository == "r"'), { format: 'npm', path: '/x', repository: '' }).result,
    ).toBe('no_match')
  })

  it('uses three valued logic: an unknown input never matches and is explained', () => {
    const byName = ok('coordinate.name == "lib"')
    const unknown = verdictOf(byName, sample('/x'))
    expect(unknown.result).toBe('unknown')
    expect(unknown.reason).toContain('coordinate.name')
    expect(verdictOf(byName, sample('/x', { coordinate: { name: 'lib' } })).result).toBe('match')
    expect(
      verdictOf(ok('coordinate.group == "g" || format == "maven2"'), sample('/x')).result,
    ).toBe('match')
    expect(verdictOf(ok('coordinate.group == "g" && format == "npm"'), sample('/x')).result).toBe(
      'no_match',
    )
    expect(
      verdictOf(ok('coordinate.group == "g" && format == "maven2"'), sample('/x')).result,
    ).toBe('unknown')
    expect(verdictOf(ok('coordinate.group == "g" || format == "npm"'), sample('/x')).result).toBe(
      'unknown',
    )
    expect(verdictOf(ok('!(coordinate.version == "1")'), sample('/x')).result).toBe('unknown')
    expect(verdictOf(ok('coordinate.version.startsWith("1")'), sample('/x')).result).toBe('unknown')
    expect(verdictOf(ok('coordinate.version in ["1"]'), sample('/x')).result).toBe('unknown')
    expect(
      verdictOf(ok('coordinate.version =~ "1"'), sample('/x', { coordinate: { version: '1' } }))
        .result,
    ).toBe('match')
  })

  it('refuses an input that is too large to evaluate', () => {
    const verdict = verdictOf(
      ok('format == "maven2"'),
      sample(`/${'a'.repeat(LIMITS.max_input_bytes)}`),
    )
    expect(verdict.result).toBe('unknown')
    expect(verdict.reason).toContain('longer')
  })
})

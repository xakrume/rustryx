import type { components } from '@/api/schema'

type Issue = components['schemas']['SelectorCompileIssue']
type Sample = components['schemas']['SelectorSample']
export type Verdict = 'match' | 'no_match' | 'unknown'

/**
 * A small stand-in for the Rust selector engine, for the in-browser mock only: it lets the editor
 * show real compile errors with byte offsets and real verdicts per sample without a backend. The
 * production engine is the authority; this one covers the documented subset (comparison, regex,
 * `in`, `startsWith`, `endsWith`, `contains`, `&&`, `||`, `!`) with the same three valued logic:
 * an unknown input (a coordinate part a sample does not have) never matches.
 */
export const LIMITS = {
  max_expression_bytes: 1024,
  max_nesting: 32,
  max_in_items: 64,
  max_regex_bytes: 512,
  max_input_bytes: 8192,
  max_eval_nodes: 4096,
} as const

export const LANGUAGE = {
  variables: [
    { name: 'format', type: 'string', description: 'Repository format, for example maven2 or npm' },
    { name: 'path', type: 'string', description: 'Normalized path with a leading slash' },
    { name: 'repository', type: 'string', description: 'Name of the repository' },
    { name: 'coordinate.group', type: 'string', description: 'Group of the component, if known' },
    { name: 'coordinate.name', type: 'string', description: 'Name of the component, if known' },
    { name: 'coordinate.version', type: 'string', description: 'Version, if known' },
  ],
  operators: [
    { syntax: 'a == "x"', description: 'Equal, a string comparison' },
    { syntax: 'a != "x"', description: 'Not equal' },
    { syntax: 'a =~ "re"', description: 'Regex must match the whole string' },
    { syntax: 'a in ["x", "y"]', description: 'Equal to one of the listed strings' },
    { syntax: 'a.startsWith("x")', description: 'Prefix test (also endsWith and contains)' },
    { syntax: '!a  a && b  a || b', description: 'Not, and, or (and binds tighter than or)' },
  ],
  limits: LIMITS,
} as const

type Token =
  { type: 'str' | 'ident' | 'op'; text: string; at: number } | { type: 'eof'; text: ''; at: number }

type Node =
  | { kind: 'str'; value: string; at: number }
  | { kind: 'list'; items: string[]; at: number }
  | { kind: 'var'; name: string; at: number }
  | { kind: 'not'; operand: Node; at: number }
  | { kind: 'bin'; op: string; left: Node; right: Node; at: number }
  | { kind: 'call'; target: Node; method: string; arg: string; at: number }

const encoder = new TextEncoder()
const byteOffset = (text: string, index: number): number =>
  encoder.encode(text.slice(0, index)).length

class CompileError extends Error {
  constructor(readonly issue: Issue) {
    super(issue.message)
  }
}

function tokenize(text: string): Token[] {
  const tokens: Token[] = []
  let i = 0
  const fail = (message: string, at: number): never => {
    throw new CompileError({ kind: 'syntax', message, offset: byteOffset(text, at) })
  }
  while (i < text.length) {
    const ch = text[i] ?? ''
    if (/\s/.test(ch)) {
      i += 1
    } else if (ch === '"') {
      let j = i + 1
      let out = ''
      while (j < text.length && text[j] !== '"') {
        if (text[j] === '\\' && j + 1 < text.length) {
          out += text[j + 1]
          j += 2
        } else {
          out += text[j]
          j += 1
        }
      }
      if (j >= text.length) fail('syntax error: unterminated string', i)
      tokens.push({ type: 'str', text: out, at: i })
      i = j + 1
    } else if (/[A-Za-z_]/.test(ch)) {
      let j = i
      while (j < text.length && /[A-Za-z0-9_.]/.test(text[j] ?? '')) j += 1
      tokens.push({ type: 'ident', text: text.slice(i, j), at: i })
      i = j
    } else {
      const two = text.slice(i, i + 2)
      if (['==', '!=', '=~', '&&', '||'].includes(two)) {
        tokens.push({ type: 'op', text: two, at: i })
        i += 2
      } else if ('()[],!'.includes(ch)) {
        tokens.push({ type: 'op', text: ch, at: i })
        i += 1
      } else {
        fail(`syntax error: unexpected character '${ch}'`, i)
      }
    }
  }
  tokens.push({ type: 'eof', text: '', at: text.length })
  return tokens
}

class Parser {
  private pos = 0
  private depth = 0
  constructor(
    private readonly tokens: Token[],
    private readonly source: string,
  ) {}

  private peek(): Token {
    return this.tokens[this.pos] ?? { type: 'eof', text: '', at: this.source.length }
  }

  private fail(message: string, token: Token = this.peek()): never {
    throw new CompileError({
      kind: 'syntax',
      message,
      offset: byteOffset(this.source, token.at),
    })
  }

  private accept(text: string): boolean {
    const token = this.peek()
    if ((token.type === 'op' || token.type === 'ident') && token.text === text) {
      this.pos += 1
      return true
    }
    return false
  }

  private expect(text: string): void {
    if (!this.accept(text)) {
      const token = this.peek()
      this.fail(
        token.type === 'eof'
          ? `syntax error: expected '${text}' but the expression ends`
          : `syntax error: unexpected token '${token.text}'`,
      )
    }
  }

  parse(): Node {
    const node = this.or()
    const token = this.peek()
    if (token.type !== 'eof') this.fail(`syntax error: unexpected token '${token.text}'`)
    return node
  }

  private nest<T>(run: () => T): T {
    this.depth += 1
    if (this.depth > LIMITS.max_nesting) {
      throw new CompileError({
        kind: 'too_deep',
        message: `expression is nested deeper than ${LIMITS.max_nesting} levels`,
        offset: byteOffset(this.source, this.peek().at),
      })
    }
    try {
      return run()
    } finally {
      this.depth -= 1
    }
  }

  private or(): Node {
    let left = this.and()
    while (this.peek().text === '||' && this.peek().type === 'op') {
      const at = this.peek().at
      this.pos += 1
      left = { kind: 'bin', op: '||', left, right: this.and(), at }
    }
    return left
  }

  private and(): Node {
    let left = this.not()
    while (this.peek().text === '&&' && this.peek().type === 'op') {
      const at = this.peek().at
      this.pos += 1
      left = { kind: 'bin', op: '&&', left, right: this.not(), at }
    }
    return left
  }

  private not(): Node {
    const token = this.peek()
    if (token.type === 'op' && token.text === '!') {
      this.pos += 1
      return { kind: 'not', operand: this.nest(() => this.not()), at: token.at }
    }
    return this.comparison()
  }

  private comparison(): Node {
    const left = this.postfix()
    const token = this.peek()
    if (token.type === 'op' && ['==', '!=', '=~'].includes(token.text)) {
      this.pos += 1
      return { kind: 'bin', op: token.text, left, right: this.postfix(), at: token.at }
    }
    if (token.type === 'ident' && token.text === 'in') {
      this.pos += 1
      return { kind: 'bin', op: 'in', left, right: this.list(), at: token.at }
    }
    return left
  }

  private list(): Node {
    const open = this.peek()
    this.expect('[')
    const items: string[] = []
    while (!(this.peek().type === 'op' && this.peek().text === ']')) {
      const token = this.peek()
      if (token.type !== 'str') this.fail(`syntax error: expected a string, found '${token.text}'`)
      items.push(token.text)
      this.pos += 1
      if (!this.accept(',')) break
    }
    this.expect(']')
    if (items.length > LIMITS.max_in_items) {
      throw new CompileError({
        kind: 'too_long',
        message: `a list may hold at most ${LIMITS.max_in_items} items`,
        offset: byteOffset(this.source, open.at),
      })
    }
    return { kind: 'list', items, at: open.at }
  }

  private postfix(): Node {
    let node = this.primary()
    // `name.startsWith("x")` is tokenized as one identifier: split the method off the variable.
    while (node.kind === 'var' && /\.(startsWith|endsWith|contains)$/.test(node.name)) {
      const method = node.name.slice(node.name.lastIndexOf('.') + 1)
      const target: Node = { ...node, name: node.name.slice(0, node.name.lastIndexOf('.')) }
      this.expect('(')
      const arg = this.peek()
      if (arg.type !== 'str') this.fail(`syntax error: ${method} takes a string`)
      this.pos += 1
      this.expect(')')
      node = { kind: 'call', target, method, arg: arg.text, at: node.at }
    }
    return node
  }

  private primary(): Node {
    const token = this.peek()
    if (token.type === 'str') {
      this.pos += 1
      return { kind: 'str', value: token.text, at: token.at }
    }
    if (token.type === 'ident') {
      this.pos += 1
      return { kind: 'var', name: token.text, at: token.at }
    }
    if (token.type === 'op' && token.text === '(') {
      this.pos += 1
      const inner = this.nest(() => this.or())
      this.expect(')')
      return inner
    }
    return this.fail(
      token.type === 'eof'
        ? 'syntax error: the expression ends unexpectedly'
        : `syntax error: unexpected token '${token.text}'`,
    )
  }
}

const VARIABLES = new Set([
  'format',
  'path',
  'repository',
  'coordinate.group',
  'coordinate.name',
  'coordinate.version',
])

type Type = 'string' | 'bool' | 'list'

function typeOf(node: Node, source: string, issues: Issue[]): Type {
  const typeIssue = (message: string, at: number): Type => {
    issues.push({ kind: 'type', message, offset: byteOffset(source, at) })
    return 'bool'
  }
  switch (node.kind) {
    case 'str':
      return 'string'
    case 'list':
      return 'list'
    case 'var':
      if (!VARIABLES.has(node.name)) {
        issues.push({
          kind: 'type',
          message: `type error: unknown variable '${node.name}'`,
          offset: byteOffset(source, node.at),
        })
      }
      return 'string'
    case 'not':
      if (typeOf(node.operand, source, issues) !== 'bool') {
        return typeIssue('type error: ! needs a boolean', node.at)
      }
      return 'bool'
    case 'call':
      if (typeOf(node.target, source, issues) !== 'string') {
        return typeIssue(`type error: ${node.method} needs a string`, node.at)
      }
      return 'bool'
    case 'bin': {
      const left = typeOf(node.left, source, issues)
      const right = typeOf(node.right, source, issues)
      if (node.op === '&&' || node.op === '||') {
        if (left !== 'bool' || right !== 'bool') {
          return typeIssue(`type error: ${node.op} needs booleans on both sides`, node.at)
        }
      } else if (node.op === 'in') {
        if (left !== 'string' || right !== 'list') {
          return typeIssue('type error: in needs a string and a list', node.at)
        }
      } else if (left !== 'string' || right !== 'string') {
        return typeIssue(`type error: ${node.op} compares strings`, node.at)
      } else if (node.op === '=~' && node.right.kind === 'str') {
        if (encoder.encode(node.right.value).length > LIMITS.max_regex_bytes) {
          issues.push({
            kind: 'regex',
            message: `regex is longer than ${LIMITS.max_regex_bytes} bytes`,
            offset: byteOffset(source, node.right.at),
          })
        } else {
          try {
            new RegExp(`^(?:${node.right.value})$`)
          } catch {
            issues.push({
              kind: 'regex',
              message: 'regex error: the pattern does not compile',
              offset: byteOffset(source, node.right.at),
            })
          }
        }
      }
      return 'bool'
    }
  }
}

export interface Compiled {
  node: Node
  source: string
}

/** Compile an expression: the compiled form, or every issue found (syntax errors stop early). */
export function compile(source: string): Compiled | Issue[] {
  const bytes = encoder.encode(source).length
  if (source.trim() === '') {
    return [{ kind: 'empty', message: 'the expression is empty', offset: null }]
  }
  if (bytes > LIMITS.max_expression_bytes) {
    return [
      {
        kind: 'too_long',
        message: `the expression is ${bytes} bytes, the limit is ${LIMITS.max_expression_bytes}`,
        offset: LIMITS.max_expression_bytes,
      },
    ]
  }
  try {
    const node = new Parser(tokenize(source), source).parse()
    const issues: Issue[] = []
    if (typeOf(node, source, issues) !== 'bool' && issues.length === 0) {
      issues.push({
        kind: 'type',
        message: 'type error: the expression must be true or false',
        offset: 0,
      })
    }
    return issues.length > 0 ? issues : { node, source }
  } catch (error) {
    if (error instanceof CompileError) return [error.issue]
    throw error
  }
}

type Tri = boolean | null

function value(node: Node, sample: Sample): string | null {
  if (node.kind === 'str') return node.value
  if (node.kind !== 'var') return null
  switch (node.name) {
    case 'format':
      return sample.format
    case 'path':
      return sample.path
    case 'repository':
      return sample.repository ?? ''
    case 'coordinate.group':
      return sample.coordinate?.group ?? null
    case 'coordinate.name':
      return sample.coordinate?.name ?? null
    default:
      return sample.coordinate?.version ?? null
  }
}

function evaluate(node: Node, sample: Sample): Tri {
  switch (node.kind) {
    case 'not': {
      const inner = evaluate(node.operand, sample)
      return inner === null ? null : !inner
    }
    case 'call': {
      const target = value(node.target, sample)
      if (target === null) return null
      if (node.method === 'startsWith') return target.startsWith(node.arg)
      if (node.method === 'endsWith') return target.endsWith(node.arg)
      return target.includes(node.arg)
    }
    case 'bin': {
      if (node.op === '&&' || node.op === '||') {
        const a = evaluate(node.left, sample)
        const b = evaluate(node.right, sample)
        if (node.op === '&&')
          return a === false || b === false ? false : a === null || b === null ? null : true
        return a === true || b === true ? true : a === null || b === null ? null : false
      }
      const left = value(node.left, sample)
      if (left === null) return null
      if (node.op === 'in') return node.right.kind === 'list' && node.right.items.includes(left)
      const right = value(node.right, sample)
      if (right === null) return null
      if (node.op === '==') return left === right
      if (node.op === '!=') return left !== right
      return new RegExp(`^(?:${right})$`).test(left)
    }
    default:
      return null
  }
}

/** Which input made a sample `unknown`, for the `reason` of a verdict. */
function unknownReason(node: Node, sample: Sample): string {
  const names: string[] = []
  const walk = (n: Node): void => {
    if (n.kind === 'var' && value(n, sample) === null) names.push(n.name)
    if (n.kind === 'not') walk(n.operand)
    if (n.kind === 'bin') {
      walk(n.left)
      walk(n.right)
    }
    if (n.kind === 'call') walk(n.target)
  }
  walk(node)
  return `the sample has no ${[...new Set(names)].join(', ') || 'input'} to decide this`
}

export function verdictOf(
  compiled: Compiled,
  sample: Sample,
): { result: Verdict; reason: string | null } {
  if (encoder.encode(sample.path).length > LIMITS.max_input_bytes) {
    return { result: 'unknown', reason: `the path is longer than ${LIMITS.max_input_bytes} bytes` }
  }
  const outcome = evaluate(compiled.node, sample)
  if (outcome === null) return { result: 'unknown', reason: unknownReason(compiled.node, sample) }
  return { result: outcome ? 'match' : 'no_match', reason: null }
}

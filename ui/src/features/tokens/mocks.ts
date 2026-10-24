import { http, HttpResponse, type HttpHandler } from 'msw'
import type { TokenRecord } from '@/features/roles/mock-seed'
import {
  handle,
  invalidInput,
  MockProblem,
  pageResponse,
  readJson,
  slice,
} from '@/features/roles/mock-http'
import {
  audit,
  commit,
  freshId,
  identity,
  isoNow,
  tokenState,
  tokenView,
} from '@/features/roles/mock-store'

const SCOPE = /^(\*|repo:(\*|[A-Za-z0-9][A-Za-z0-9._-]*):(browse|read|write|delete|\*))$/
const ALPHABET = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789'
const MAX_TTL_DAYS = 365

let secrets = 0
/** A deterministic stand-in for a token: `ryx_` plus 43 characters, never a real credential. */
function mockSecret(): string {
  secrets += 1
  let state = 7919 * secrets
  let out = ''
  for (let i = 0; i < 43; i += 1) {
    state = (state * 1664525 + 1013904223) % 4294967296
    out += ALPHABET[state % ALPHABET.length]
  }
  return `ryx_${out}`
}

function listTokens(request: Request, owner: string | null): Response {
  const url = new URL(request.url)
  const state = url.searchParams.get('state')
  const kind = url.searchParams.get('kind')
  const neverUsed = url.searchParams.get('never_used')
  const expiringBefore = url.searchParams.get('expiring_before')
  const ownerId = owner ?? url.searchParams.get('owner_id')
  const items = identity()
    .tokens.filter((t) => ownerId === null || t.user_id === ownerId)
    .filter((t) => state === null || tokenState(t) === state)
    .filter((t) => kind === null || t.kind === kind)
    .filter((t) => neverUsed !== 'true' || t.last_used_at === null)
    .filter(
      (t) =>
        expiringBefore === null ||
        (t.expires_at !== null && Date.parse(t.expires_at) < Date.parse(expiringBefore)),
    )
    .sort((a, b) => b.created_at.localeCompare(a.created_at))
    .map(tokenView)
  return pageResponse(slice(items, url, { defaultLimit: 100 }))
}

function revoke(tokenId: string, owner: string | null): TokenRecord {
  const token = identity().tokens.find(
    (t) => t.id === tokenId && (owner === null || t.user_id === owner),
  )
  if (!token) throw new MockProblem(404, 'api_token_not_found', 'The token does not exist')
  // Idempotent: revoking a revoked token answers 200 again with the same record.
  const next = token.revoked_at === null ? { ...token, revoked_at: isoNow() } : token
  commit({ tokens: identity().tokens.map((t) => (t.id === token.id ? next : t)) })
  return next
}

/** Stateful mock of personal and administrator token endpoints. The secret is returned once. */
export const handlers: HttpHandler[] = [
  http.get('/api/v1/me/tokens', ({ request }) =>
    handle(request, { feature: 'tokens', admin: false }, (caller) =>
      listTokens(request, caller.userId ?? 'none'),
    ),
  ),

  http.post('/api/v1/me/tokens', ({ request }) =>
    handle(request, { feature: 'tokens', admin: false }, async (caller) => {
      const body = await readJson<{ name?: unknown; expires_in_days?: unknown; scopes?: unknown }>(
        request,
      )
      const name = typeof body.name === 'string' ? body.name.trim() : ''
      const days = body.expires_in_days === undefined ? 90 : Number(body.expires_in_days)
      const scopes = Array.isArray(body.scopes) ? body.scopes : []
      const fields: { field: string; code: string; message: string }[] = []
      if (name === '' || name.length > 128) {
        fields.push({
          field: '/name',
          code: 'invalid',
          message: 'The name must be 1 to 128 characters.',
        })
      }
      if (!Number.isInteger(days) || days < 1 || days > MAX_TTL_DAYS) {
        fields.push({
          field: '/expires_in_days',
          code: 'out_of_range',
          message: `The lifetime must be between 1 and ${MAX_TTL_DAYS} days.`,
        })
      }
      if (scopes.length > 64 || scopes.some((s) => typeof s !== 'string' || !SCOPE.test(s))) {
        fields.push({ field: '/scopes', code: 'invalid', message: 'A scope is malformed.' })
      }
      if (fields.length > 0) throw invalidInput(fields)
      const secret = mockSecret()
      const token: TokenRecord = {
        id: freshId(),
        user_id: caller.userId ?? '',
        name,
        display_prefix: secret.slice(0, 8),
        scopes: scopes as string[],
        expires_at: new Date(Date.now() + days * 86_400_000)
          .toISOString()
          .replace(/\.\d{3}Z$/, 'Z'),
        revoked_at: null,
        last_used_at: null,
        created_at: isoNow(),
        kind: 'personal',
      }
      commit({ tokens: [token, ...identity().tokens] })
      audit(caller, {
        kind: 'token_change',
        action: 'token.create',
        target: name,
        detail: { expires_in_days: days },
      })
      return HttpResponse.json(
        { ...tokenView(token), secret },
        { status: 201, headers: { Location: '/' } },
      )
    }),
  ),

  http.post('/api/v1/me/tokens/:tokenId/actions/revoke', ({ request, params }) =>
    handle(request, { feature: 'tokens', admin: false }, (caller) => {
      const token = revoke(String(params.tokenId), caller.userId ?? 'none')
      audit(caller, { kind: 'token_change', action: 'token.revoke', target: token.name })
      return HttpResponse.json(tokenView(token))
    }),
  ),

  http.get('/api/v1/api-tokens', ({ request }) =>
    handle(request, { feature: 'tokens' }, () => listTokens(request, null)),
  ),

  http.post('/api/v1/api-tokens/:tokenId/actions/revoke', ({ request, params }) =>
    handle(request, { feature: 'tokens' }, (caller) => {
      const token = revoke(String(params.tokenId), null)
      audit(caller, { kind: 'token_change', action: 'token.revoke', target: token.name })
      return HttpResponse.json(tokenView(token))
    }),
  ),
]

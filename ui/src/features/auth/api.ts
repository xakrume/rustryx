import { queryOptions, useMutation, useQueryClient } from '@tanstack/react-query'
import { useNavigate } from '@tanstack/react-router'
import { api } from '@/api/client'
import { setCsrfToken } from '@/api/csrf'
import { STALE_TIME } from '@/api/query-client'
import { unwrap } from '@/api/request'
import type { components } from '@/api/schema'

export type Session = components['schemas']['Session']
export type LoginProviders = components['schemas']['LoginProviders']

/** Query keys of the principal. Everything about "who am I" is cleared together on logout. */
export const authKeys = {
  all: ['auth'] as const,
  session: ['auth', 'session'] as const,
  me: ['auth', 'me'] as const,
  providers: ['auth', 'providers'] as const,
}

/**
 * The session answers 200 for anonymous callers too (`authenticated: false`), so a failure
 * here is an outage, not "not signed in". Reading it also arms the CSRF token for every later
 * mutation.
 */
export const sessionQuery = queryOptions({
  queryKey: authKeys.session,
  queryFn: async ({ signal }): Promise<Session> => {
    const session = await unwrap(api.GET('/api/v1/auth/session', { signal }))
    setCsrfToken(session.csrf_token)
    return session
  },
  staleTime: STALE_TIME.principal,
})

export const meQuery = queryOptions({
  queryKey: authKeys.me,
  queryFn: ({ signal }) => unwrap(api.GET('/api/v1/me', { signal })),
  staleTime: STALE_TIME.principal,
})

export const providersQuery = queryOptions({
  queryKey: authKeys.providers,
  queryFn: ({ signal }) => unwrap(api.GET('/api/v1/auth/providers', { signal })),
  staleTime: STALE_TIME.static,
  // A failed provider list degrades the login page, it does not block it.
  retry: 1,
})

export interface LoginInput {
  username: string
  password: string
}

export function useLogin() {
  const queryClient = useQueryClient()
  return useMutation({
    // The login form shows its own errors (generic message, lock-out countdown).
    meta: { silent: true },
    mutationFn: async (input: LoginInput): Promise<Session> => {
      const session = await unwrap(api.POST('/api/v1/auth/login', { body: input }))
      setCsrfToken(session.csrf_token)
      return session
    },
    onSuccess: (session) => {
      queryClient.setQueryData(authKeys.session, session)
      // Principal data of the previous visitor (the anonymous /me answer) must not survive
      // into the new session: remove it, so the shell fetches the signed-in principal fresh.
      queryClient.removeQueries({ queryKey: authKeys.me })
    },
  })
}

export interface LogoutResult {
  /** Keycloak end-session URL when RP-initiated logout is configured, else null. */
  redirectUrl: string | null
}

export function useLogout() {
  const queryClient = useQueryClient()
  return useMutation({
    meta: { silent: true },
    mutationFn: async (): Promise<LogoutResult> => {
      const result = await unwrap(api.POST('/api/v1/auth/logout'))
      return { redirectUrl: result.redirect_url }
    },
    onSettled: () => {
      // Even when the call failed the user asked to leave: forget the token and every cached
      // answer that was authorized for the old principal.
      setCsrfToken(null)
      queryClient.clear()
    },
  })
}

/**
 * The one sign-out path for every entry point (user menu, command palette, anything later).
 *
 * Two entry points used to end the session differently: one followed the end-session URL
 * of the identity provider, the other only went to the login page, which left the Keycloak
 * session alive and let the next "Sign in with Keycloak" click log the user straight back
 * in. Keeping the navigation next to `useLogout` makes it impossible to forget half of it.
 * `onError` is the place to report a failed call; the local session state is already dropped
 * by `useLogout`, so the user is signed out of this app either way.
 */
export function useSignOut(onError?: (error: unknown) => void): {
  signOut: () => void
  isPending: boolean
} {
  const logout = useLogout()
  const navigate = useNavigate()
  const signOut = () =>
    logout.mutate(undefined, {
      onSuccess: ({ redirectUrl }) => {
        if (redirectUrl) window.location.assign(redirectUrl)
        else void navigate({ to: '/login', replace: true })
      },
      onError: (error) => {
        onError?.(error)
        void navigate({ to: '/login', replace: true })
      },
    })
  return { signOut, isPending: logout.isPending }
}

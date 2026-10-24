import { api } from '@/api/client'
import { unwrap } from '@/api/request'

/** `PUT /me/password`: 204 and the session cookie is reissued; no password is ever returned. */
export async function changeMyPassword(current: string, next: string): Promise<void> {
  await unwrap(
    api.PUT('/api/v1/me/password', { body: { current_password: current, new_password: next } }),
  )
}

/** `POST /me/actions/sign-out-everywhere`: every session of the caller ends, cookies are cleared. */
export async function signOutEverywhere(): Promise<void> {
  await unwrap(api.POST('/api/v1/me/actions/sign-out-everywhere'))
}

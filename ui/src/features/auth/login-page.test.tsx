import { screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { expectNoA11yViolations, renderApp } from '@/test/utils'

afterEach(() => vi.restoreAllMocks())

async function fillAndSubmit(username: string, password: string) {
  const user = userEvent.setup()
  await user.type(await screen.findByLabelText(/^Username/), username)
  await user.type(screen.getByLabelText(/^Password/), password)
  await user.click(screen.getByRole('button', { name: 'Sign in' }))
  return user
}

describe('login page', () => {
  it('shows the local form and the single sign-on button, with no accessibility violations', async () => {
    const { container } = renderApp('/login')
    expect(
      await screen.findByRole('heading', { level: 1, name: 'Sign in to Rustryx' }),
    ).toBeInTheDocument()
    expect(await screen.findByRole('button', { name: 'Sign in with Keycloak' })).toBeEnabled()
    expect(screen.getByLabelText(/^Username/)).toHaveFocus()
    await expectNoA11yViolations(container)
  })

  it('signs in and goes to the page the user was heading for', async () => {
    const { router } = renderApp('/login?return_to=%2Frepositories')
    await fillAndSubmit('admin', 'mock-password')
    await waitFor(() => expect(router.state.location.pathname).toBe('/repositories'))
    expect(
      await screen.findByRole('heading', { level: 1, name: 'Repositories' }),
    ).toBeInTheDocument()
  })

  it('must_not_redirect_to_another_site_through_return_to', async () => {
    const { router } = renderApp('/login?return_to=%2F%2Fevil.example%2Fphish')
    await fillAndSubmit('admin', 'mock-password')
    await waitFor(() => expect(router.state.location.pathname).toBe('/'))
    expect(router.state.location.href).not.toContain('evil.example')
  })

  it('shows ONE generic message for wrong credentials and keeps the username', async () => {
    renderApp('/login')
    await fillAndSubmit('admin', 'wrong-password')
    expect(await screen.findByText('The username or password is not correct.')).toBeInTheDocument()
    expect(screen.getByLabelText(/^Username/)).toHaveValue('admin')
    expect(screen.getByLabelText(/^Password/)).toHaveValue('')
    expect(screen.getByLabelText(/^Password/)).toHaveFocus()
    // The same text for an account that does not exist: no user enumeration.
    const user = userEvent.setup()
    await user.clear(screen.getByLabelText(/^Username/))
    await user.type(screen.getByLabelText(/^Username/), 'nobody')
    await user.type(screen.getByLabelText(/^Password/), 'whatever')
    await user.click(screen.getByRole('button', { name: 'Sign in' }))
    await waitFor(() =>
      expect(screen.getAllByText('The username or password is not correct.')).toHaveLength(1),
    )
  })

  it('shows a lock-out countdown from Retry-After and blocks submit meanwhile', async () => {
    renderApp('/login')
    await fillAndSubmit('locked', 'x')
    expect(
      await screen.findByText(/Too many failed attempts\. Try again in \d+ s\./),
    ).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Sign in' })).toBeDisabled()
  })

  it('says sign in is unavailable when the login service is down, and keeps the form', async () => {
    renderApp('/login')
    await fillAndSubmit('unavailable', 'x')
    expect(await screen.findByText(/Sign in is temporarily unavailable/)).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Sign in' })).toBeEnabled()
  })

  it('validates required fields next to the field before any request', async () => {
    const user = userEvent.setup()
    renderApp('/login')
    await user.click(await screen.findByRole('button', { name: 'Sign in' }))
    expect(await screen.findByText('Enter your username.')).toBeInTheDocument()
    expect(screen.getByText('Enter your password.')).toBeInTheDocument()
  })

  it('disables the SSO button with an explanation when the provider is unreachable, local form stays', async () => {
    window.sessionStorage.setItem('ryx-mock-sso-down', '1')
    renderApp('/login')
    const button = await screen.findByRole('button', { name: 'Sign in with Keycloak' })
    expect(button).toBeDisabled()
    expect(button).toHaveAccessibleDescription(/Single sign-on is unavailable/)
    expect(screen.getByLabelText(/^Username/)).toBeEnabled()
  })

  it('starts the SSO redirect as a top level navigation to the server path with return_to', async () => {
    const assign = vi.fn()
    vi.stubGlobal('location', { ...window.location, assign })
    const user = userEvent.setup()
    renderApp('/login?return_to=%2Frepositories')
    await user.click(await screen.findByRole('button', { name: 'Sign in with Keycloak' }))
    expect(assign).toHaveBeenCalledWith('/api/v1/auth/oidc/login?return_to=%2Frepositories')
    vi.unstubAllGlobals()
  })

  it('shows a failed SSO callback with the request id', async () => {
    renderApp('/login?error=oidc_callback_invalid&request_id=req-abc')
    expect(
      await screen.findByText(/single sign-on attempt expired or was invalid/i),
    ).toBeInTheDocument()
    expect(screen.getByText('req-abc')).toBeInTheDocument()
    expect(screen.getByRole('button', { name: 'Copy Request ID' })).toBeInTheDocument()
  })

  it('tells the user that the session expired when sent here by a 401', async () => {
    renderApp('/login?reason=expired')
    expect(await screen.findByText(/session has expired/i)).toBeInTheDocument()
  })

  it('goes straight to the destination when already signed in', async () => {
    const { router } = renderApp('/login?return_to=%2Fsearch', { user: 'admin' })
    await waitFor(() => expect(router.state.location.pathname).toBe('/search'))
  })

  it('toggles password visibility with an accessible state', async () => {
    const user = userEvent.setup()
    renderApp('/login')
    const password = await screen.findByLabelText(/^Password/)
    expect(password).toHaveAttribute('type', 'password')
    const toggle = screen.getByRole('button', { name: 'Show password' })
    await user.click(toggle)
    expect(password).toHaveAttribute('type', 'text')
    expect(screen.getByRole('button', { name: 'Hide password' })).toHaveAttribute(
      'aria-pressed',
      'true',
    )
  })
})

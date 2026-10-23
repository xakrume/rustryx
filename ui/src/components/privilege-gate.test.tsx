import { screen } from '@testing-library/react'
import { describe, expect, it } from 'vitest'
import { handlers } from '@/mocks/handlers'
import { startSession } from '@/mocks/session'
import { renderWithProviders } from '@/test/utils'
import { server } from '@/test/server'
import { PrivilegeGate } from './privilege-gate'

function setup(user: 'admin' | 'alice' | null, ui: React.ReactElement) {
  server.use(...handlers)
  if (user) startSession(user)
  return renderWithProviders(ui)
}

describe('PrivilegeGate', () => {
  it('shows its children to a principal that has the privilege', async () => {
    setup('admin', <PrivilegeGate need="admin">{<button>Delete repository</button>}</PrivilegeGate>)
    expect(await screen.findByRole('button', { name: 'Delete repository' })).toBeInTheDocument()
  })

  it('must_not_render_the_children_without_the_privilege (hidden, not disabled)', async () => {
    setup(
      'alice',
      <>
        <PrivilegeGate need="admin" fallback={<p>fallback shown</p>}>
          <button>Delete repository</button>
        </PrivilegeGate>
        <PrivilegeGate need={{ can: 'upload' }}>
          <p>upload allowed</p>
        </PrivilegeGate>
      </>,
    )
    expect(await screen.findByText('upload allowed')).toBeInTheDocument()
    expect(screen.queryByRole('button', { name: 'Delete repository' })).not.toBeInTheDocument()
    expect(screen.getByText('fallback shown')).toBeInTheDocument()
  })

  it('hands the decision to a render function for disabled-with-reason controls', async () => {
    setup(
      'alice',
      <PrivilegeGate need="admin">
        {(allowed) => (
          <>
            <button disabled={!allowed}>Delete</button>
            {!allowed ? <p>Needs the nx-admin role.</p> : null}
          </>
        )}
      </PrivilegeGate>,
    )
    expect(await screen.findByRole('button', { name: 'Delete' })).toBeDisabled()
    expect(screen.getByText('Needs the nx-admin role.')).toBeInTheDocument()
  })

  it('treats an anonymous visitor as having no privilege', async () => {
    setup(
      null,
      <PrivilegeGate need="authenticated" fallback={<p>please sign in</p>}>
        <p>secret page</p>
      </PrivilegeGate>,
    )
    expect(await screen.findByText('please sign in')).toBeInTheDocument()
    expect(screen.queryByText('secret page')).not.toBeInTheDocument()
  })
})

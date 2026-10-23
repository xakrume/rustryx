import { useState } from 'react'
import { ApiError } from '@/api/errors'
import { CodeBlock } from '@/components/code-block'
import { ConfirmDialog } from '@/components/confirm-dialog'
import { ConflictDialog } from '@/components/conflict-dialog'
import { ErrorPanel } from '@/components/error-panel'
import { ForbiddenPanel } from '@/components/forbidden-panel'
import { SecretOnce } from '@/components/secret-once'
import { Button } from '@/components/ui/button'
import { toast } from '@/lib/toast-store'
import { DemoSection } from '../demo-section'

const REQUEST_ID = '0192f6c4-5a1e-7000-8000-00000000c0de'

const notFound = new ApiError({
  code: 'repository_not_found',
  message: 'Repository not found',
  status: 404,
  requestId: REQUEST_ID,
})
const throttled = new ApiError({
  code: 'rate_limited',
  message: 'Too many requests',
  status: 429,
  requestId: REQUEST_ID,
  retryAfterSecs: 15,
})
const serverFailure = new ApiError({
  code: 'internal_error',
  message: 'The server failed to handle the request',
  status: 500,
  requestId: REQUEST_ID,
})
const offline = new ApiError({ code: 'network_error', message: 'offline', status: 0 })
const inUse = new ApiError({
  code: 'resource_in_use',
  message: 'The routing rule is still assigned to repositories',
  status: 409,
  requestId: REQUEST_ID,
  details: {
    dependents: [
      { kind: 'repository', id: '0192f6c4-5a1e-7000-8000-000000000015', name: 'npm-proxy' },
      { kind: 'repository', id: '0192f6c4-5a1e-7000-8000-000000000016', name: 'pypi-proxy' },
    ],
  },
})

function wait(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms))
}

export function FeedbackSection() {
  const [confirm, setConfirm] = useState(false)
  const [typed, setTyped] = useState(false)
  const [failing, setFailing] = useState(false)
  const [secret, setSecret] = useState<string | null>(null)
  const [stale, setStale] = useState(false)
  const [state, setState] = useState(false)

  return (
    <DemoSection
      id="feedback"
      title="Toasts, dialogs, errors"
      description="Every failure shows the stable error code and the request id with copy buttons."
    >
      <div className="flex flex-wrap gap-2">
        <Button
          variant="outline"
          onClick={() => toast.success('Repository created', { description: 'maven-releases' })}
        >
          Success toast
        </Button>
        <Button variant="outline" onClick={() => toast.info('Upload queued')}>
          Info toast
        </Button>
        <Button variant="outline" onClick={() => toast.warning('Quota almost reached')}>
          Warning toast
        </Button>
        <Button
          variant="outline"
          onClick={() => toast.apiError(serverFailure, 'Could not delete the role')}
        >
          API error toast
        </Button>
        <Button variant="outline" onClick={() => setConfirm(true)}>
          Confirm dialog
        </Button>
        <Button variant="outline" onClick={() => setTyped(true)}>
          Typed confirmation
        </Button>
        <Button variant="outline" onClick={() => setFailing(true)}>
          Confirm that fails (409)
        </Button>
        <Button
          variant="outline"
          onClick={() => setSecret('ryx_Qk3Tm9Zp0aLw7Vb2XyHn5CdEf8GjRs1UoIq4KtAx6Ml')}
        >
          Show a secret once
        </Button>
        <Button variant="outline" onClick={() => setStale(true)}>
          Conflict 412
        </Button>
        <Button variant="outline" onClick={() => setState(true)}>
          Conflict 409
        </Button>
      </div>

      <div className="grid gap-3 lg:grid-cols-2">
        <ErrorPanel error={notFound} onRetry={() => toast.info('Retrying')} />
        <ErrorPanel error={throttled} onRetry={() => toast.info('Retrying')} />
        <ErrorPanel error={serverFailure} onRetry={() => toast.info('Retrying')} />
        <ErrorPanel error={offline} onRetry={() => toast.info('Retrying')} />
      </div>
      <ForbiddenPanel privilege="app.users.read (phase 1: nx-admin)" />

      <div className="grid gap-3 lg:grid-cols-2">
        <CodeBlock
          label="Maven command"
          value="mvn deploy -DaltDeploymentRepository=rustryx::https://repo.example.org/repository/maven-releases/"
        />
        <CodeBlock
          label="API token (hidden until shown)"
          value="ryx_Qk3Tm9Zp0aLw7Vb2XyHn5CdEf8GjRs1UoIq4KtAx6Ml"
          secret
        />
      </div>

      <ConfirmDialog
        open={confirm}
        onOpenChange={setConfirm}
        kind="role"
        name="maven-publishers"
        onConfirm={() => wait(500).then(() => toast.success('Role deleted'))}
      />
      <ConfirmDialog
        open={typed}
        onOpenChange={setTyped}
        kind="repository"
        name="maven-releases"
        requireTyping
        confirmLabel="Delete repository"
        onConfirm={() => wait(500).then(() => toast.success('Repository deleted'))}
      />
      <ConfirmDialog
        open={failing}
        onOpenChange={setFailing}
        kind="routing rule"
        name="block-snapshots"
        onConfirm={() => wait(300).then(() => Promise.reject(inUse))}
      />
      <SecretOnce secret={secret} what="API token" onDone={() => setSecret(null)} />
      <ConflictDialog
        open={stale}
        kind="stale"
        resourceName="role maven-publishers"
        error={
          new ApiError({
            code: 'precondition_failed',
            message: 'Changed',
            status: 412,
            requestId: REQUEST_ID,
          })
        }
        onReload={() => setStale(false)}
        onOverwrite={() => setStale(false)}
        onCopyMine={() => toast.info('Copied')}
        onClose={() => setStale(false)}
      />
      <ConflictDialog
        open={state}
        kind="state"
        resourceName="routing rule block-snapshots"
        error={inUse}
        onReload={() => setState(false)}
        onClose={() => setState(false)}
      />
    </DemoSection>
  )
}

import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { KeyRound, X } from 'lucide-react'
import { useId, useMemo, useState } from 'react'
import { ErrorPanel } from '@/components/error-panel'
import { SecretOnce } from '@/components/secret-once'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Label } from '@/components/ui/label'
import { useMe, usePermissions } from '@/features/auth/use-permissions'
import { t } from '@/i18n'
import { cn } from '@/lib/utils'
import { clientSetupQuery, type ClientSetup, type ClientSnippet } from './api'
import { CreateClientTokenDialog } from './create-token-dialog'
import { fillPlaceholders, SnippetBlock } from './snippet-block'

/**
 * The tools of the snippets, grouped into the tabs a user thinks in: gradle sits with Maven,
 * twine with pip. A client the table does not know gets a tab of its own, named after it.
 */
const FAMILY: Readonly<Record<string, string>> = {
  mvn: 'maven',
  gradle: 'maven',
  npm: 'npm',
  pip: 'pip',
  twine: 'pip',
  docker: 'docker',
  helm: 'helm',
  dart: 'dart',
  curl: 'raw',
}
const KNOWN_FAMILIES = ['maven', 'npm', 'pip', 'docker', 'helm', 'dart', 'raw'] as const

function familyOf(client: string): string {
  return FAMILY[client] ?? client
}

function familyLabel(family: string): string {
  return (KNOWN_FAMILIES as readonly string[]).includes(family)
    ? t(`clientSetup.families.${family as (typeof KNOWN_FAMILIES)[number]}`)
    : family
}

export interface ClientSetupPanelProps {
  /** Repository name. */
  repository: string
  /** Hide the panel heading when the surrounding drawer or card already has one. */
  hideTitle?: boolean
  className?: string
}

/**
 * The reusable `client setup` panel (docs/ui-page-inventory.md, section 9): the server builds
 * the snippets, this renders them per tool with copy buttons and marked placeholders. Nothing
 * secret is ever in a snippet from the server. "Create a token for me" shows the new token once
 * and fills it into the snippets of THIS page view; it lives in component state only, so
 * reloading the page removes it and nothing is written to the browser.
 */
export function ClientSetupPanel({
  repository,
  hideTitle = false,
  className,
}: ClientSetupPanelProps) {
  const headingId = useId()
  const query = useQuery(clientSetupQuery(repository))
  const { data } = query

  return (
    <section
      aria-labelledby={hideTitle ? undefined : headingId}
      className={cn('space-y-3', className)}
    >
      {hideTitle ? null : (
        <h2 id={headingId} className="text-base font-semibold">
          {t('clientSetup.title')}
        </h2>
      )}
      {data ? (
        <PanelBody setup={data} />
      ) : query.error ? (
        <ErrorPanel
          error={query.error}
          title={t('clientSetup.loadFailed')}
          onRetry={() => void query.refetch()}
        />
      ) : (
        <div aria-busy="true" className="space-y-2">
          <Skeleton className="h-8 w-64" />
          <Skeleton className="h-28" />
        </div>
      )}
    </section>
  )
}

function Notes({ setup }: { setup: ClientSetup }) {
  const permissions = usePermissions()
  return (
    <div className="space-y-2">
      <Alert variant="info">
        {setup.variant === 'anonymous'
          ? t('clientSetup.variantAnonymous')
          : t('clientSetup.variantAuthenticated')}
      </Alert>
      {setup.publish_target ? (
        <Alert variant="info">
          {t('clientSetup.publishTarget', { repository: setup.publish_target.repository })}{' '}
          <Link
            to="/repositories/$name"
            params={{ name: setup.publish_target.repository }}
            className="font-medium underline underline-offset-2"
          >
            {setup.publish_target.repository}
          </Link>
          {setup.publish_target.reason ? <span> {setup.publish_target.reason}</span> : null}
        </Alert>
      ) : null}
      {!setup.base_url_configured ? (
        <Alert variant="warning">
          {t('clientSetup.baseUrlMissing', { url: setup.base_url })}{' '}
          {permissions.isAdmin ? (
            <Link to="/admin/settings" className="font-medium underline underline-offset-2">
              {t('clientSetup.setBaseUrl')}
            </Link>
          ) : null}
        </Alert>
      ) : null}
      {setup.private_ca ? <Alert variant="warning">{t('clientSetup.privateCa')}</Alert> : null}
      {setup.notes.map((note) => (
        <Alert key={note.text} variant={note.kind === 'warning' ? 'warning' : 'info'}>
          {note.text}
        </Alert>
      ))}
    </div>
  )
}

interface PanelBodyProps {
  setup: ClientSetup
}

function PanelBody({ setup }: PanelBodyProps) {
  const me = useMe()
  const permissions = usePermissions()
  const [raw, setRaw] = useState(false)
  const [tokenDialogOpen, setTokenDialogOpen] = useState(false)
  // The one time view of the secret, and the copy used in the snippets of this page view.
  const [shownOnce, setShownOnce] = useState<string | null>(null)
  const [snippetToken, setSnippetToken] = useState<string | null>(null)
  const rawId = useId()

  const families = useMemo(() => {
    const groups = new Map<string, ClientSnippet[]>()
    for (const snippet of setup.snippets) {
      const family = familyOf(snippet.client)
      groups.set(family, [...(groups.get(family) ?? []), snippet])
    }
    return [...groups.entries()]
  }, [setup.snippets])
  const [active, setActive] = useState<string | null>(null)
  const activeFamily =
    active !== null && families.some(([family]) => family === active) ? active : families[0]?.[0]

  const username = me?.user?.username ?? null
  const values: Record<string, string> = {
    ...(snippetToken === null ? {} : { TOKEN: snippetToken }),
    ...(snippetToken !== null && username !== null ? { USERNAME: username } : {}),
  }
  const canMint = permissions.can.create_personal_token && me?.auth_method !== 'api_token'
  const canWrite = setup.snippets.some((s) => s.requires === 'write')
  const placeholders = [
    ...new Map(
      setup.snippets.flatMap((s) => s.placeholders).map((p) => [p.name, p] as const),
    ).values(),
  ]
  const policy = me?.token_policy

  if (families.length === 0) {
    return (
      <div className="space-y-2">
        <Notes setup={setup} />
        <Alert variant="info">
          {t('clientSetup.noSnippets', {
            url: `${setup.base_url}/repository/${setup.repository.name}/`,
          })}
        </Alert>
      </div>
    )
  }

  return (
    <div className="space-y-3">
      <Notes setup={setup} />
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex items-center gap-2">
          <Switch id={rawId} checked={raw} onCheckedChange={setRaw} />
          <Label htmlFor={rawId}>{t('clientSetup.showRaw')}</Label>
        </div>
        <div className="flex flex-wrap items-center gap-2">
          {snippetToken !== null ? (
            <Button variant="outline" size="sm" onClick={() => setSnippetToken(null)}>
              <X aria-hidden="true" />
              {t('clientSetup.removeToken')}
            </Button>
          ) : null}
          {canMint ? (
            <Button variant="outline" size="sm" onClick={() => setTokenDialogOpen(true)}>
              <KeyRound aria-hidden="true" />
              {t('clientSetup.createToken')}
            </Button>
          ) : null}
        </div>
      </div>
      {snippetToken !== null ? (
        <Alert variant="success">{t('clientSetup.tokenInUse')}</Alert>
      ) : null}

      <Tabs value={activeFamily} onValueChange={setActive}>
        <div className="max-w-full overflow-x-auto">
          <TabsList aria-label={t('clientSetup.clients')}>
            {families.map(([family]) => (
              <TabsTrigger key={family} value={family}>
                {familyLabel(family)}
              </TabsTrigger>
            ))}
          </TabsList>
        </div>
        {families.map(([family, snippets]) => (
          <TabsContent key={family} value={family} className="space-y-3">
            {snippets.map((snippet) => {
              const usesToken =
                snippetToken !== null && snippet.placeholders.some((p) => p.name === 'TOKEN')
              return (
                <SnippetBlock
                  key={snippet.id}
                  snippet={snippet}
                  content={fillPlaceholders(snippet.content, values)}
                  secret={usesToken}
                  raw={raw}
                />
              )
            })}
          </TabsContent>
        ))}
      </Tabs>

      {placeholders.length > 0 && snippetToken === null ? (
        <div className="text-xs text-muted-foreground">
          <p className="font-medium text-foreground">{t('clientSetup.placeholders')}</p>
          <p>{t('clientSetup.placeholderHelp')}</p>
          <dl className="mt-1 grid grid-cols-[auto_1fr] gap-x-3 gap-y-0.5">
            {placeholders.map((p) => (
              <div key={p.name} className="contents">
                <dt className="font-mono text-foreground">{`<${p.name}>`}</dt>
                <dd>{p.description}</dd>
              </div>
            ))}
          </dl>
        </div>
      ) : null}

      <CreateClientTokenDialog
        open={tokenDialogOpen}
        onOpenChange={setTokenDialogOpen}
        repository={setup.repository.name}
        canWrite={canWrite}
        defaultTtlDays={policy?.default_ttl_days ?? 90}
        maxTtlDays={policy?.max_ttl_days ?? 365}
        onCreated={({ secret }) => {
          setShownOnce(secret)
          setSnippetToken(secret)
        }}
      />
      <SecretOnce
        secret={shownOnce}
        what={t('clientSetup.tokenWhat')}
        onDone={() => setShownOnce(null)}
      />
    </div>
  )
}

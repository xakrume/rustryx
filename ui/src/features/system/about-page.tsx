import { useQuery } from '@tanstack/react-query'
import { ExternalLink } from 'lucide-react'
import { CodeBlock } from '@/components/code-block'
import { CopyButton } from '@/components/copy-button'
import { ErrorPanel } from '@/components/error-panel'
import { PageHeader } from '@/components/page-header'
import { RelativeTime } from '@/components/relative-time'
import { Alert } from '@/components/ui/alert'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { t } from '@/i18n'
import { systemInfoQuery } from './api'

/** Only web addresses and paths of this server are linked: a `javascript:` value would be text. */
function safeHref(value: string | null): string | null {
  if (value === null) return null
  if (value.startsWith('/') && !value.startsWith('//')) return value
  try {
    const url = new URL(value)
    return url.protocol === 'https:' || url.protocol === 'http:' ? url.toString() : null
  } catch {
    return null
  }
}

function Item({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <div>
      <dt className="text-sm text-muted-foreground">{label}</dt>
      <dd className="text-sm">{children}</dd>
    </div>
  )
}

function ExternalLinkText({ href, children }: { href: string; children: React.ReactNode }) {
  return (
    <a
      href={href}
      target="_blank"
      rel="noopener noreferrer"
      className="inline-flex items-center gap-1 text-link underline underline-offset-2"
    >
      {children}
      <ExternalLink className="size-3.5" aria-hidden="true" />
      <span className="sr-only">{t('aboutPage.supply.newTab')}</span>
    </a>
  )
}

/**
 * `/about`: what this server is. It needs no sign in (the information is public by contract:
 * nothing that helps an attacker) and is the page to quote when reporting a problem: version,
 * commit, build time, and how to check the SBOM and the image signature.
 */
export function AboutPage() {
  const query = useQuery(systemInfoQuery)
  const info = query.data
  const sbom = safeHref(info?.supply_chain.sbom_url ?? null)
  const licenses = safeHref(info?.third_party_licenses_url ?? null)

  return (
    <>
      <PageHeader title={t('aboutPage.title')} description={t('aboutPage.description')} />
      {query.isError && !info ? (
        <ErrorPanel
          error={query.error}
          onRetry={() => void query.refetch()}
          title={t('aboutPage.loadFailed')}
        />
      ) : !info ? (
        <div aria-busy="true" className="min-h-48 space-y-3">
          <Skeleton className="h-28" />
          <Skeleton className="h-28" />
        </div>
      ) : (
        <div className="grid max-w-4xl gap-4">
          {info.banner ? (
            <Alert variant="info" title={t('aboutPage.banner')}>
              {info.banner}
            </Alert>
          ) : null}
          <Card>
            <CardHeader>
              <CardTitle>{t('aboutPage.product.title')}</CardTitle>
            </CardHeader>
            <CardContent>
              <dl className="grid gap-x-8 gap-y-3 sm:grid-cols-3">
                <Item label={t('aboutPage.product.name')}>{info.name}</Item>
                <Item label={t('aboutPage.product.version')}>
                  <span className="font-mono">{info.version}</span>
                </Item>
                <Item label={t('aboutPage.product.license')}>{info.license}</Item>
              </dl>
            </CardContent>
          </Card>
          <Card>
            <CardHeader>
              <CardTitle>{t('aboutPage.build.title')}</CardTitle>
            </CardHeader>
            <CardContent>
              <dl className="grid gap-x-8 gap-y-3 sm:grid-cols-3">
                <Item label={t('aboutPage.build.commit')}>
                  {info.build.commit ? (
                    <span className="inline-flex items-center gap-1">
                      <span className="font-mono">{info.build.commit}</span>
                      <CopyButton
                        value={info.build.commit}
                        what={t('aboutPage.build.commit')}
                        size="icon"
                        className="size-6"
                      />
                    </span>
                  ) : (
                    t('aboutPage.build.unknown')
                  )}
                </Item>
                <Item label={t('aboutPage.build.builtAt')}>
                  {info.build.built_at ? (
                    <RelativeTime value={info.build.built_at} />
                  ) : (
                    t('aboutPage.build.unknown')
                  )}
                </Item>
                <Item label={t('aboutPage.build.profile')}>
                  <span className="font-mono">
                    {info.build.profile ?? t('aboutPage.build.unknown')}
                  </span>
                </Item>
              </dl>
            </CardContent>
          </Card>
          <Card>
            <CardHeader>
              <CardTitle>{t('aboutPage.supply.title')}</CardTitle>
            </CardHeader>
            <CardContent className="space-y-4">
              <dl className="grid gap-x-8 gap-y-3 sm:grid-cols-2">
                <Item label={t('aboutPage.supply.sbom')}>
                  {sbom ? (
                    <ExternalLinkText href={sbom}>
                      {t('aboutPage.supply.sbomLink')}
                    </ExternalLinkText>
                  ) : (
                    <span className="text-muted-foreground">{t('aboutPage.supply.sbomNone')}</span>
                  )}
                </Item>
                <Item label={t('aboutPage.supply.licenses')}>
                  {licenses ? (
                    <ExternalLinkText href={licenses}>
                      {t('aboutPage.supply.licensesLink')}
                    </ExternalLinkText>
                  ) : (
                    <span className="text-muted-foreground">
                      {t('aboutPage.supply.licensesNone')}
                    </span>
                  )}
                </Item>
              </dl>
              {info.supply_chain.signature_hint ? (
                <CodeBlock
                  value={info.supply_chain.signature_hint}
                  label={t('aboutPage.supply.signature')}
                  wrap
                />
              ) : (
                <p className="text-sm text-muted-foreground">
                  {t('aboutPage.supply.signatureNone')}
                </p>
              )}
            </CardContent>
          </Card>
        </div>
      )}
    </>
  )
}

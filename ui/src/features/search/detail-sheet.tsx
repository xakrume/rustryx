import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { ExternalLink } from 'lucide-react'
import { BytesText } from '@/components/bytes-text'
import { ErrorPanel } from '@/components/error-panel'
import { RelativeTime } from '@/components/relative-time'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Sheet,
  SheetContent,
  SheetDescription,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import { assetQuery, componentLabel, componentQuery } from '@/features/components/api'
import { AssetDetailsView, SectionTitle } from '@/features/components/asset-details'
import { Fact, FactList } from '@/features/components/fact-list'
import { ClientSetupPanel } from '@/features/client-setup/client-setup-panel'
import { t } from '@/i18n'

export type Selection =
  { kind: 'component'; id: string; title: string } | { kind: 'asset'; id: string; title: string }

function Setup({ repository }: { repository: string }) {
  return (
    <details className="rounded-md border border-border p-3">
      <summary className="cursor-pointer text-sm font-semibold">{t('clientSetup.title')}</summary>
      <div className="pt-3">
        <ClientSetupPanel repository={repository} hideTitle />
      </div>
    </details>
  )
}

function ComponentBody({ id }: { id: string }) {
  const query = useQuery(componentQuery(id))
  const component = query.data
  if (component === undefined) {
    return query.error ? (
      <ErrorPanel error={query.error} onRetry={() => void query.refetch()} />
    ) : (
      <Skeleton className="h-40" aria-hidden="true" />
    )
  }
  return (
    <div className="space-y-4">
      <Button asChild variant="outline" size="sm">
        <Link to="/components/$id" params={{ id: component.id }}>
          <ExternalLink aria-hidden="true" />
          {t('search.openComponent')}
        </Link>
      </Button>
      <section>
        <SectionTitle level={3}>{t('components.summary')}</SectionTitle>
        <FactList>
          <Fact label={t('components.name')} mono>
            {componentLabel(component)}
          </Fact>
          <Fact label={t('components.repository')}>
            <Link
              to="/repositories/$name"
              params={{ name: component.repository }}
              className="font-mono text-link underline-offset-2 hover:underline"
            >
              {component.repository}
            </Link>
          </Fact>
          <Fact label={t('components.size')}>
            <BytesText bytes={component.size_bytes} />
          </Fact>
          <Fact label={t('components.assetCount')}>{component.asset_count.toLocaleString()}</Fact>
          <Fact label={t('components.updated')}>
            <RelativeTime value={component.updated_at} />
          </Fact>
        </FactList>
      </section>
      <section>
        <SectionTitle level={3}>{t('components.assets')}</SectionTitle>
        <ul className="m-0 list-none space-y-1 p-0 text-sm">
          {component.assets.slice(0, 8).map((asset) => (
            <li key={asset.id}>
              <Link
                to="/assets/$id"
                params={{ id: asset.id }}
                className="font-mono text-[0.8125rem] break-all text-link underline-offset-2 hover:underline"
              >
                {asset.path}
              </Link>
            </li>
          ))}
        </ul>
      </section>
      <Setup repository={component.repository} />
    </div>
  )
}

function AssetBody({ id }: { id: string }) {
  const query = useQuery(assetQuery(id))
  if (query.data === undefined) {
    return query.error ? (
      <ErrorPanel error={query.error} onRetry={() => void query.refetch()} />
    ) : (
      <Skeleton className="h-40" aria-hidden="true" />
    )
  }
  return (
    <div className="space-y-4">
      <AssetDetailsView asset={query.data} level={3} showOpenLink />
      <Setup repository={query.data.repository} />
    </div>
  )
}

/**
 * The detail pane of a search hit. It is a drawer so it works the same on a phone and on a wide
 * screen: the table keeps its width, and Escape or the close button returns focus to the row's
 * button. It shows what the browse pane shows, plus the client setup of the repository.
 */
export function DetailSheet({
  selection,
  onClose,
}: {
  selection: Selection | null
  onClose: () => void
}) {
  return (
    <Sheet open={selection !== null} onOpenChange={(open) => !open && onClose()}>
      <SheetContent side="right" className="sm:w-[40rem]">
        <SheetHeader className="border-b border-border p-4 pr-14">
          <SheetTitle className="font-mono text-base break-all">
            {selection?.title ?? ''}
          </SheetTitle>
          <SheetDescription>{t('search.detailBody')}</SheetDescription>
        </SheetHeader>
        <div className="min-h-0 flex-1 overflow-y-auto p-4">
          {selection?.kind === 'component' ? <ComponentBody id={selection.id} /> : null}
          {selection?.kind === 'asset' ? <AssetBody id={selection.id} /> : null}
        </div>
      </SheetContent>
    </Sheet>
  )
}

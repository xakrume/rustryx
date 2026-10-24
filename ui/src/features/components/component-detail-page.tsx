import { useQuery } from '@tanstack/react-query'
import { Link, useNavigate, useParams } from '@tanstack/react-router'
import { FolderTree, Trash2 } from 'lucide-react'
import { useState } from 'react'
import { BytesText } from '@/components/bytes-text'
import { CopyButton } from '@/components/copy-button'
import { ErrorPanel } from '@/components/error-panel'
import { PageHeader } from '@/components/page-header'
import { RelativeTime } from '@/components/relative-time'
import { Alert } from '@/components/ui/alert'
import { Badge } from '@/components/ui/badge'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import {
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableHeader,
  TableRow,
} from '@/components/ui/table'
import { FormatBadge } from '@/features/repositories/repo-meta'
import { t } from '@/i18n'
import { componentLabel, componentName, componentQuery, type ComponentDetail } from './api'
import { useContentPermissions } from './asset-details'
import { AttributeList } from './attribute-list'
import { failureTitle } from './error-title'
import { DeleteComponentDialog } from './delete-dialogs'
import { Fact, FactList } from './fact-list'

function directoryOf(path: string): string {
  const slash = path.lastIndexOf('/')
  return slash < 0 ? '/' : `/${path.slice(0, slash + 1)}`
}

function AssetsTable({ component }: { component: ComponentDetail }) {
  return (
    <Card className="lg:col-span-2">
      <CardHeader>
        <CardTitle>{t('components.assets')}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        <Table label={t('components.assets')} containerClassName="max-h-96">
          <TableHeader>
            <TableRow className="hover:bg-muted">
              <TableHead scope="col">{t('components.path')}</TableHead>
              <TableHead scope="col">{t('components.contentType')}</TableHead>
              <TableHead scope="col" className="text-right">
                {t('components.size')}
              </TableHead>
              <TableHead scope="col">SHA-256</TableHead>
              <TableHead scope="col">{t('components.updated')}</TableHead>
            </TableRow>
          </TableHeader>
          <TableBody>
            {component.assets.map((asset) => (
              <TableRow key={asset.id}>
                <TableCell className="whitespace-nowrap">
                  <Link
                    to="/assets/$id"
                    params={{ id: asset.id }}
                    className="font-mono text-link underline-offset-2 hover:underline"
                  >
                    {asset.path}
                  </Link>
                </TableCell>
                <TableCell className="font-mono whitespace-nowrap text-muted-foreground">
                  {asset.content_type}
                </TableCell>
                <TableCell className="text-right whitespace-nowrap tabular-nums">
                  <BytesText bytes={asset.size_bytes} />
                </TableCell>
                <TableCell className="whitespace-nowrap">
                  <span className="flex items-center gap-1">
                    <code
                      className="font-mono text-muted-foreground"
                      title={asset.checksums.sha256}
                    >
                      {asset.checksums.sha256.slice(0, 12)}
                    </code>
                    <CopyButton
                      value={asset.checksums.sha256}
                      what={t('components.checksumOf', { algorithm: 'SHA-256' })}
                    />
                  </span>
                </TableCell>
                <TableCell className="whitespace-nowrap">
                  <RelativeTime value={asset.updated_at} />
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
        {component.assets_truncated ? (
          <Alert variant="info">{t('components.assetsTruncated')}</Alert>
        ) : null}
      </CardContent>
    </Card>
  )
}

/** `/components/:id`: the package version, its files, its attributes and the delete action. */
export function ComponentDetailPage() {
  const { id } = useParams({ from: '/_app/components/$id' })
  const navigate = useNavigate()
  const [deleting, setDeleting] = useState(false)
  const query = useQuery(componentQuery(id))
  const component = query.data
  const permissions = useContentPermissions(component?.repository ?? '')

  if (component === undefined) {
    return (
      <>
        <PageHeader title={t('pages.componentDetail')} />
        {query.error ? (
          <ErrorPanel
            error={query.error}
            title={failureTitle(query.error, t('components.componentLoadFailed'))}
            onRetry={() => void query.refetch()}
          />
        ) : (
          <div aria-busy="true" className="space-y-3">
            <Skeleton className="h-10 w-96" />
            <Skeleton className="h-56" />
          </div>
        )}
      </>
    )
  }

  const first = component.assets[0]
  const label = componentLabel(component)
  const tags = component.attributes.tags
  return (
    <>
      <PageHeader
        title={componentName(component)}
        description={
          <span className="flex flex-wrap items-center gap-1.5">
            {component.version ? (
              <Badge variant="neutral" className="font-mono">
                {component.version}
              </Badge>
            ) : null}
            <FormatBadge format={component.format} />
            <Link
              to="/repositories/$name"
              params={{ name: component.repository }}
              className="font-mono text-link underline-offset-2 hover:underline"
            >
              {component.repository}
            </Link>
          </span>
        }
        actions={
          <>
            {first && component.format !== 'docker' && component.format !== 'oci' ? (
              <Button asChild variant="outline">
                <Link
                  to="/repositories/$name/browse"
                  params={{ name: component.repository }}
                  search={{ path: directoryOf(first.path) }}
                >
                  <FolderTree aria-hidden="true" />
                  {t('components.showInTree')}
                </Link>
              </Button>
            ) : null}
            {permissions.delete ? (
              <Button variant="destructive" onClick={() => setDeleting(true)}>
                <Trash2 aria-hidden="true" />
                {t('common.delete')}
              </Button>
            ) : null}
          </>
        }
      />
      <div className="grid gap-4 lg:grid-cols-2">
        <Card>
          <CardHeader>
            <CardTitle>{t('components.summary')}</CardTitle>
          </CardHeader>
          <CardContent>
            <FactList>
              <Fact label={t('components.name')} mono>
                {component.name}
              </Fact>
              {component.group_id ? (
                <Fact label={t('components.namespace')} mono>
                  {component.group_id}
                </Fact>
              ) : null}
              <Fact label={t('components.version')} mono>
                {component.version ?? '-'}
              </Fact>
              {Array.isArray(tags) && tags.length > 0 ? (
                <Fact label={t('components.tags')} mono>
                  {tags.join(', ')}
                </Fact>
              ) : null}
              <Fact label={t('components.size')}>
                <BytesText bytes={component.size_bytes} />
              </Fact>
              <Fact label={t('components.assetCount')}>
                {component.asset_count.toLocaleString()}
              </Fact>
              <Fact label={t('components.created')}>
                <RelativeTime value={component.created_at} />
              </Fact>
              <Fact label={t('components.updated')}>
                <RelativeTime value={component.updated_at} />
              </Fact>
            </FactList>
          </CardContent>
        </Card>
        <Card>
          <CardHeader>
            <CardTitle>{t('components.attributes')}</CardTitle>
          </CardHeader>
          <CardContent>
            <AttributeList attributes={component.attributes} />
          </CardContent>
        </Card>
        <AssetsTable component={component} />
      </div>
      <DeleteComponentDialog
        component={deleting ? { id: component.id, label, assets: component.asset_count } : null}
        onClose={() => setDeleting(false)}
        onDeleted={() =>
          void navigate({
            to: '/repositories/$name/components',
            params: { name: component.repository },
            search: {},
          })
        }
      />
    </>
  )
}

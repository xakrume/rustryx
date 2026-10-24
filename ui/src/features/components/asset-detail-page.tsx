import { useQuery } from '@tanstack/react-query'
import { Link, useNavigate, useParams } from '@tanstack/react-router'
import { ErrorPanel } from '@/components/error-panel'
import { PageHeader } from '@/components/page-header'
import { Card, CardContent } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { t } from '@/i18n'
import { assetQuery } from './api'
import { AssetDetailsView } from './asset-details'
import { failureTitle } from './error-title'

/**
 * `/assets/:id`. The page asks the server to verify the blob (`check_blob=true`), because a file
 * whose bytes are gone is an integrity problem the page must be able to say out loud.
 */
export function AssetDetailPage() {
  const { id } = useParams({ from: '/_app/assets/$id' })
  const navigate = useNavigate()
  const query = useQuery(assetQuery(id, true))
  const asset = query.data
  const fileName = asset?.path.split('/').pop() ?? t('pages.assetDetail')

  return (
    <>
      <PageHeader
        title={fileName}
        description={
          asset ? (
            <Link
              to="/repositories/$name"
              params={{ name: asset.repository }}
              className="font-mono text-link underline-offset-2 hover:underline"
            >
              {asset.repository}
            </Link>
          ) : undefined
        }
      />
      {asset ? (
        <Card>
          <CardContent className="p-4">
            <AssetDetailsView
              asset={asset}
              onDeleted={() =>
                void navigate({
                  to: '/repositories/$name/components',
                  params: { name: asset.repository },
                  search: {},
                })
              }
            />
          </CardContent>
        </Card>
      ) : query.error ? (
        <ErrorPanel
          error={query.error}
          title={failureTitle(query.error, t('components.assetLoadFailed'))}
          onRetry={() => void query.refetch()}
        />
      ) : (
        <div aria-busy="true" className="space-y-3">
          <Skeleton className="h-10 w-80" />
          <Skeleton className="h-64" />
        </div>
      )}
    </>
  )
}

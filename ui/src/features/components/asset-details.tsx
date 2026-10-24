import { useQuery } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { Download, ExternalLink, Trash2, TriangleAlert } from 'lucide-react'
import { createElement, useState, type ReactNode } from 'react'
import { BytesText } from '@/components/bytes-text'
import { CopyButton } from '@/components/copy-button'
import { RelativeTime } from '@/components/relative-time'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { usePermissions } from '@/features/auth/use-permissions'
import { repositoryQuery } from '@/features/repositories/api'
import { t } from '@/i18n'
import type { AssetDetail } from './api'
import { AttributeList } from './attribute-list'
import { ChecksumList } from './checksum-list'
import { DeleteAssetDialog } from './delete-dialogs'
import { Fact, FactList } from './fact-list'

export function SectionTitle({ level, children }: { level: 2 | 3; children: ReactNode }) {
  return createElement(
    `h${level}`,
    { className: 'mb-2 text-base leading-6 font-semibold' },
    children,
  )
}

/** What the caller may do with files of a repository: the repository's own permissions when known. */
export function useContentPermissions(repository: string): { read: boolean; delete: boolean } {
  const permissions = usePermissions()
  // Without a name there is nothing to ask: the empty path would be the repository list.
  const repo = useQuery({ ...repositoryQuery(repository), enabled: repository !== '' }).data?.data
  return {
    read: repo?.permissions.read ?? permissions.canOnRepository(repository, 'read'),
    delete: repo?.permissions.delete ?? permissions.canOnRepository(repository, 'delete'),
  }
}

export interface AssetDetailsViewProps {
  asset: AssetDetail
  /** Heading level of the sections: 2 on a page of its own, 3 inside the browse pane. */
  level?: 2 | 3
  /** Offer a link to the asset's own page (the browse pane does, the page itself does not). */
  showOpenLink?: boolean
  onDeleted?: () => void
}

/**
 * Everything the contract knows about one file: where it is, how big, when it was published and
 * downloaded, its three checksums with copy buttons, the format attributes, and the actions
 * (download, copy the URL, delete). Used by the asset page and by the detail pane of the browse
 * tree, so both say the same things the same way. A blob that is missing in the store is an
 * integrity warning, not a quiet absence.
 */
export function AssetDetailsView({
  asset,
  level = 2,
  showOpenLink = false,
  onDeleted,
}: AssetDetailsViewProps) {
  const permissions = usePermissions()
  const content = useContentPermissions(asset.repository)
  const [deleting, setDeleting] = useState(false)
  return (
    <div className="space-y-4">
      {asset.blob_state === 'missing' ? (
        <Alert variant="destructive" title={t('components.blobMissingTitle')}>
          {t('components.blobMissingBody')}{' '}
          {permissions.isAdmin ? (
            <Link to="/admin/tasks" className="font-medium underline underline-offset-2">
              {t('components.blobMissingLink')}
            </Link>
          ) : null}
        </Alert>
      ) : null}

      <div className="flex flex-wrap items-center gap-2">
        {content.read ? (
          <Button asChild variant="outline" size="sm">
            <a href={asset.download_url} download>
              <Download aria-hidden="true" />
              {t('components.download')}
            </a>
          </Button>
        ) : null}
        <CopyButton
          value={asset.download_url}
          what={t('components.urlOfFile')}
          showLabel
          variant="outline"
        />
        {showOpenLink ? (
          <Button asChild variant="outline" size="sm">
            <Link to="/assets/$id" params={{ id: asset.id }}>
              <ExternalLink aria-hidden="true" />
              {t('components.openAsset')}
            </Link>
          </Button>
        ) : null}
        {content.delete ? (
          <Button variant="destructive" size="sm" onClick={() => setDeleting(true)}>
            <Trash2 aria-hidden="true" />
            {t('common.delete')}
          </Button>
        ) : null}
      </div>

      <section>
        <SectionTitle level={level}>{t('components.summary')}</SectionTitle>
        <FactList>
          <Fact label={t('components.path')} mono>
            {asset.path}
          </Fact>
          <Fact label={t('components.repository')}>
            <Link
              to="/repositories/$name"
              params={{ name: asset.repository }}
              className="font-mono text-link underline-offset-2 hover:underline"
            >
              {asset.repository}
            </Link>
          </Fact>
          {asset.component_id ? (
            <Fact label={t('components.component')}>
              <Link
                to="/components/$id"
                params={{ id: asset.component_id }}
                className="text-link underline-offset-2 hover:underline"
              >
                {t('components.openComponent')}
              </Link>
            </Fact>
          ) : null}
          <Fact label={t('components.contentType')} mono>
            {asset.content_type}
          </Fact>
          <Fact label={t('components.size')}>
            <BytesText bytes={asset.size_bytes} />
          </Fact>
          <Fact label={t('components.uploaded')}>
            <RelativeTime value={asset.created_at} />
          </Fact>
          <Fact label={t('components.updated')}>
            <RelativeTime value={asset.updated_at} />
          </Fact>
          <Fact label={t('components.lastDownloaded')}>
            {asset.last_downloaded_at ? (
              <RelativeTime value={asset.last_downloaded_at} />
            ) : (
              t('components.neverDownloaded')
            )}
          </Fact>
          <Fact label={t('components.downloads30d')}>
            {asset.download_count_30d === null
              ? t('components.notCounted')
              : asset.download_count_30d.toLocaleString()}
          </Fact>
          <Fact label={t('components.uploader')}>
            {asset.uploader ? asset.uploader.username : t('components.uploaderHidden')}
          </Fact>
          <Fact label={t('components.blobStore')} mono>
            {asset.blob.blob_store}
          </Fact>
          <Fact label={t('components.blob')} mono>
            {asset.blob.id}
          </Fact>
          {asset.blob_state === 'missing' ? (
            <Fact label={t('components.integrity')}>
              <span className="inline-flex items-center gap-1 text-destructive-text">
                <TriangleAlert className="size-4" aria-hidden="true" />
                {t('components.blobMissingShort')}
              </span>
            </Fact>
          ) : null}
        </FactList>
      </section>

      <section>
        <SectionTitle level={level}>{t('components.checksums')}</SectionTitle>
        <ChecksumList checksums={asset.checksums} />
      </section>

      <section>
        <SectionTitle level={level}>{t('components.attributes')}</SectionTitle>
        <AttributeList attributes={asset.attributes} />
      </section>

      <DeleteAssetDialog
        asset={deleting ? { id: asset.id, path: asset.path } : null}
        onClose={() => setDeleting(false)}
        onDeleted={onDeleted}
      />
    </div>
  )
}

import { createFileRoute } from '@tanstack/react-router'
import { text } from '@/features/repositories/search-params'
import { UploadPage } from '@/features/upload/upload-page'

export const Route = createFileRoute('/_app/upload')({
  validateSearch: (search: Record<string, unknown>): { repository?: string } => ({
    repository: text(search.repository),
  }),
  staticData: { crumb: 'nav.items.upload', access: { can: 'upload' } },
  component: UploadPage,
})

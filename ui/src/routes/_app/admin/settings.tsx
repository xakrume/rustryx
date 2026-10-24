import { createFileRoute } from '@tanstack/react-router'
import { SettingsPage } from '@/features/settings/settings-page'
import { text } from '@/features/system/search-params'

interface SettingsSearch {
  /** The section tab: general, security, storage. */
  section?: string
}

export const Route = createFileRoute('/_app/admin/settings')({
  staticData: { crumb: 'nav.items.settings', access: { area: 'settings' } },
  validateSearch: (search: Record<string, unknown>): SettingsSearch => ({
    section: text(search.section),
  }),
  component: SettingsPage,
})

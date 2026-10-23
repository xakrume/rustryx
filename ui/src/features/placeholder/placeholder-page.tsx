import { Construction } from 'lucide-react'
import { EmptyState } from '@/components/empty-state'
import { PageHeader } from '@/components/page-header'
import { t, type MessageKey } from '@/i18n'

export interface PlaceholderPageProps {
  titleKey: MessageKey
  route: string
  milestone: 'M1' | 'M2' | 'M3'
}

/**
 * The page of the plan that is not built yet. Every M1 route of docs/ui-page-inventory.md
 * exists in src/routes with one of these, so the route tree, the navigation and the breadcrumbs
 * are final and a page agent only REPLACES the component of its own route file (no new route,
 * no change to the generated route tree, no merge conflicts between page agents).
 */
export function PlaceholderPage({ titleKey, route, milestone }: PlaceholderPageProps) {
  return (
    <>
      <PageHeader title={t(titleKey)} />
      <EmptyState
        icon={Construction}
        title={t('placeholder.title')}
        description={
          <>
            {t('placeholder.body')}
            <span className="mt-2 block font-mono text-xs">
              {t('placeholder.route', { route })}
            </span>
            <span className="block text-xs">{t('placeholder.milestone', { milestone })}</span>
          </>
        }
      />
    </>
  )
}

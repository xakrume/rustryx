import { useQuery } from '@tanstack/react-query'
import { ErrorPanel } from '@/components/error-panel'
import { PageHeader } from '@/components/page-header'
import { Skeleton } from '@/components/ui/skeleton'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useSearchParam, useSearchPatch } from '@/features/system/search-params'
import { t } from '@/i18n'
import { settingsSchemaQuery, type SectionName } from './api'
import { SectionForm } from './section-form'

/** The sections this version shows, in this order; a server may know more, they follow. */
const ORDER = ['general', 'security', 'storage'] as const
const KNOWN = new Set<string>(['general', 'security', 'storage', 'email', 'http-proxy', 'projects'])

function labelOf(name: string): string {
  return name in { general: 1, security: 1, storage: 1, email: 1, 'http-proxy': 1, projects: 1 }
    ? t(`systemSettings.sections.${name as 'general'}`)
    : name
}

/**
 * `/admin/settings`: one tab per settings section, generated from the section schemas the
 * server publishes. Fields that come from the configuration are shown with their source and
 * cannot be edited; the rest are forms with the ETag of what was read.
 */
export function SettingsPage() {
  const schema = useQuery(settingsSchemaQuery)
  const requested = useSearchParam('section')
  const patchSearch = useSearchPatch()
  const names = Object.keys(schema.data?.sections ?? {}).filter((n) => KNOWN.has(n))
  const ordered = [
    ...ORDER.filter((n) => names.includes(n)),
    ...names.filter((n) => !(ORDER as readonly string[]).includes(n)),
  ]
  const current = requested && ordered.includes(requested) ? requested : ordered[0]

  return (
    <>
      <PageHeader title={t('systemSettings.title')} description={t('systemSettings.description')} />
      {schema.isError && !schema.data ? (
        <ErrorPanel
          error={schema.error}
          onRetry={() => void schema.refetch()}
          title={t('systemSettings.loadFailed')}
        />
      ) : !schema.data || current === undefined ? (
        <Skeleton className="h-48 max-w-3xl" aria-busy="true" />
      ) : (
        <Tabs
          value={current}
          onValueChange={(next) => patchSearch({ section: next }, { replace: true })}
        >
          <TabsList aria-label={t('systemSettings.tabsLabel')}>
            {ordered.map((name) => (
              <TabsTrigger key={name} value={name}>
                {labelOf(name)}
              </TabsTrigger>
            ))}
          </TabsList>
          {ordered.map((name) => (
            <TabsContent key={name} value={name}>
              <SectionForm name={name as SectionName} />
            </TabsContent>
          ))}
        </Tabs>
      )}
    </>
  )
}

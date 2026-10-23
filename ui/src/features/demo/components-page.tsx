import { PageHeader } from '@/components/page-header'
import { t } from '@/i18n'
import { BasicsSection } from './sections/basics-section'
import { ChartSection } from './sections/chart-section'
import { FeedbackSection } from './sections/feedback-section'
import { FormSection } from './sections/form-section'
import { TableSection } from './sections/table-section'
import { TreeSection } from './sections/tree-section'

/**
 * Component gallery at /__components. It exists only in development and mock builds (the
 * route and this module are removed from the production bundle, see routes/_app/[_]_components.tsx)
 * and is the page to open for visual checks of the design tokens in both themes, both
 * densities and at the three layout widths.
 */
export default function ComponentsPage() {
  return (
    <>
      <PageHeader
        title={t('pages.componentsDemo')}
        description="Every shared component in one place. Switch theme and density in the top bar."
      />
      <div className="space-y-10">
        <BasicsSection />
        <FeedbackSection />
        <FormSection />
        <TableSection />
        <TreeSection />
        <ChartSection />
      </div>
    </>
  )
}

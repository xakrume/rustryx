import { Link, useMatches } from '@tanstack/react-router'
import { ChevronRight } from 'lucide-react'
import { t } from '@/i18n'

/**
 * Breadcrumb trail from the matched routes that declare `staticData.crumb`. The last crumb is
 * the current page (aria-current) and not a link. Names of dynamic objects (a repository)
 * come from the route params, so a crumb never needs a fetch.
 */
export function Breadcrumbs() {
  const matches = useMatches()
  const crumbs = matches.flatMap((match) => {
    const crumb = match.staticData.crumb
    if (crumb === undefined) return []
    const label = typeof crumb === 'function' ? crumb({ params: match.params }) : t(crumb)
    return [{ id: match.id, label, pathname: match.pathname }]
  })
  // One crumb would only repeat the page heading: a trail needs a hierarchy.
  if (crumbs.length < 2) return null
  return (
    <nav aria-label={t('a11y.breadcrumbs')} className="min-w-0">
      <ol className="m-0 flex min-w-0 list-none items-center gap-1 p-0 text-sm">
        {crumbs.map((crumb, index) => {
          const last = index === crumbs.length - 1
          return (
            <li
              key={crumb.id}
              className={`flex min-w-0 items-center gap-1 ${last ? '' : 'max-sm:hidden'}`}
            >
              {index > 0 ? (
                <ChevronRight
                  className="size-3.5 shrink-0 text-muted-foreground max-sm:hidden"
                  aria-hidden="true"
                />
              ) : null}
              {last ? (
                <span aria-current="page" className="truncate font-medium text-foreground">
                  {crumb.label}
                </span>
              ) : (
                <Link
                  to={crumb.pathname}
                  className="truncate text-muted-foreground hover:text-foreground hover:underline"
                >
                  {crumb.label}
                </Link>
              )}
            </li>
          )
        })}
      </ol>
    </nav>
  )
}

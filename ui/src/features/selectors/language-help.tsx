import { useQuery } from '@tanstack/react-query'
import { t } from '@/i18n'
import { selectorLanguageQuery } from './api'

const LIMIT_KEYS = [
  'max_expression_bytes',
  'max_nesting',
  'max_in_items',
  'max_regex_bytes',
  'max_input_bytes',
  'max_eval_nodes',
] as const

/**
 * The language reference as the engine reports it (`GET /content-selectors/language`), so the
 * help cannot drift from the compiler: variables, operators and the limits that protect the
 * server (SEC-18). A native disclosure, closed by default, works with every input method.
 */
export function LanguageHelp() {
  const language = useQuery(selectorLanguageQuery)
  return (
    <details className="rounded-md border border-border p-3 text-sm">
      <summary className="cursor-pointer font-medium">{t('selectors.help.title')}</summary>
      {language.isError ? (
        <p className="mt-2 text-muted-foreground">{t('selectors.help.loadFailed')}</p>
      ) : language.data ? (
        <div className="mt-2 space-y-3">
          <section aria-labelledby="help-variables">
            <h4 id="help-variables" className="text-xs font-semibold">
              {t('selectors.help.variables')}
            </h4>
            <dl className="mt-1 space-y-1">
              {language.data.variables.map((v) => (
                <div key={v.name}>
                  <dt className="font-mono text-xs">
                    {v.name} <span className="text-muted-foreground">({v.type})</span>
                  </dt>
                  <dd className="text-xs text-muted-foreground">{v.description}</dd>
                </div>
              ))}
            </dl>
          </section>
          <section aria-labelledby="help-operators">
            <h4 id="help-operators" className="text-xs font-semibold">
              {t('selectors.help.operators')}
            </h4>
            <dl className="mt-1 space-y-1">
              {language.data.operators.map((o) => (
                <div key={o.syntax}>
                  <dt className="font-mono text-xs">{o.syntax}</dt>
                  <dd className="text-xs text-muted-foreground">{o.description}</dd>
                </div>
              ))}
            </dl>
          </section>
          <section aria-labelledby="help-limits">
            <h4 id="help-limits" className="text-xs font-semibold">
              {t('selectors.help.limits')}
            </h4>
            <dl className="mt-1 grid grid-cols-[1fr_auto] gap-x-4 gap-y-0.5 text-xs">
              {LIMIT_KEYS.map((key) => (
                <div key={key} className="contents">
                  <dt className="text-muted-foreground">{t(`selectors.help.limitNames.${key}`)}</dt>
                  <dd className="text-right font-mono tabular-nums">{language.data.limits[key]}</dd>
                </div>
              ))}
            </dl>
          </section>
        </div>
      ) : null}
    </details>
  )
}

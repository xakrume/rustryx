import { Badge } from '@/components/ui/badge'
import { t } from '@/i18n'
import { Fact, FactList } from './fact-list'

function isPrimitive(value: unknown): value is string | number | boolean | null {
  return value === null || ['string', 'number', 'boolean'].includes(typeof value)
}

function Value({ value }: { value: unknown }) {
  if (isPrimitive(value)) return <>{value === null ? '-' : String(value)}</>
  if (Array.isArray(value) && value.every(isPrimitive)) {
    return (
      <span className="flex flex-wrap gap-1">
        {value.map((item, index) => (
          <Badge key={`${String(item)}-${index}`} variant="neutral" className="font-mono">
            {String(item)}
          </Badge>
        ))}
      </span>
    )
  }
  return (
    <pre className="m-0 overflow-x-auto rounded-md bg-muted p-2 text-[0.8125rem] whitespace-pre-wrap break-all">
      {JSON.stringify(value, null, 2)}
    </pre>
  )
}

/**
 * Format attributes (Maven packaging, npm dist-tags, OCI digest and tags, ...) as facts. The API
 * never puts secrets into attributes (docs/api-conventions.md), and the panel renders values as
 * text only, never as markup.
 */
export function AttributeList({ attributes }: { attributes: Readonly<Record<string, unknown>> }) {
  const entries = Object.entries(attributes)
  if (entries.length === 0) {
    return <p className="text-sm text-muted-foreground">{t('components.noAttributes')}</p>
  }
  return (
    <FactList>
      {entries.map(([key, value]) => (
        <Fact key={key} label={key} mono={isPrimitive(value)}>
          <Value value={value} />
        </Fact>
      ))}
    </FactList>
  )
}

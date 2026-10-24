import { CopyButton } from '@/components/copy-button'
import { t } from '@/i18n'

export interface Checksums {
  sha256: string
  sha1: string
  md5: string
}

const ROWS = [
  { key: 'sha256', label: 'SHA-256' },
  { key: 'sha1', label: 'SHA-1' },
  { key: 'md5', label: 'MD5' },
] as const

/**
 * The three digests of a blob, each with a copy button. Digests are public facts of a file, not
 * secrets, so they are plain text; they break at any character instead of widening the page.
 */
export function ChecksumList({ checksums }: { checksums: Checksums }) {
  return (
    <dl className="space-y-2 text-sm">
      {ROWS.map(({ key, label }) => (
        <div key={key} className="grid gap-x-3 gap-y-0.5 sm:grid-cols-[4.5rem_minmax(0,1fr)]">
          <dt className="text-muted-foreground sm:pt-1.5">{label}</dt>
          <dd className="flex min-w-0 items-start gap-1">
            <code className="min-w-0 pt-1.5 font-mono text-[0.8125rem] break-all">
              {checksums[key]}
            </code>
            <CopyButton
              value={checksums[key]}
              what={t('components.checksumOf', { algorithm: label })}
              className="size-8 shrink-0"
            />
          </dd>
        </div>
      ))}
    </dl>
  )
}

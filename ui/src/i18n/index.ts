import { en } from './en'

/**
 * Message lookup with typed keys. A catalog entry is a string, or a `{ one, other }` object
 * for plurals. `{name}` placeholders are replaced from `params`; a plural needs `count`.
 *
 * One language today: the catalog is imported statically. A second language later swaps the
 * `catalog` below for a locale aware loader; call sites do not change.
 */
type PluralMessage = { readonly one: string; readonly other: string }
type Leaf = string | PluralMessage

type Paths<T> = {
  [K in keyof T & string]: T[K] extends Leaf ? K : `${K}.${Paths<T[K]>}`
}[keyof T & string]

export type MessageKey = Paths<typeof en>
export type MessageParams = Readonly<Record<string, string | number>>

const catalog: typeof en = en
const pluralRules = new Intl.PluralRules('en')

function lookup(key: string): Leaf | undefined {
  let node: unknown = catalog
  for (const part of key.split('.')) {
    if (node === null || typeof node !== 'object') return undefined
    node = (node as Record<string, unknown>)[part]
  }
  if (typeof node === 'string') return node
  if (node !== null && typeof node === 'object' && 'other' in node) return node as PluralMessage
  return undefined
}

function interpolate(template: string, params: MessageParams | undefined): string {
  if (!params) return template
  return template.replace(/\{(\w+)\}/g, (whole, name: string) => {
    const value = params[name]
    return value === undefined ? whole : String(value)
  })
}

/** Translate a catalog key. An unknown key (only possible through a cast) returns the key. */
export function t(key: MessageKey, params?: MessageParams): string {
  const message = lookup(key)
  if (message === undefined) return key
  if (typeof message === 'string') return interpolate(message, params)
  const count = Number(params?.count ?? 0)
  const category = pluralRules.select(count)
  return interpolate(category === 'one' ? message.one : message.other, params)
}

/** Hook form of `t`, so components read like they will when the locale becomes reactive. */
export function useT(): typeof t {
  return t
}

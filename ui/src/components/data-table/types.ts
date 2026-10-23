import '@tanstack/react-table'

declare module '@tanstack/react-table' {
  // The generic parameters must match the library's declaration even when unused.
  // eslint-disable-next-line @typescript-eslint/no-unused-vars -- required by declaration merging
  interface ColumnMeta<TData, TValue> {
    /** Human name for the column chooser; defaults to a string header. */
    label?: string
    /**
     * API sort field. A column with a sortKey is sortable: ascending sends `key`, descending
     * sends `-key` (docs/api-conventions.md, section 7). No sortKey means not sortable.
     */
    sortKey?: string
    align?: 'left' | 'right'
    /** False pins the column: it cannot be hidden (the name column). */
    hideable?: boolean
    /** Extra classes for the th and td, for example a fixed width. */
    className?: string
  }
}

export { createColumnHelper } from '@tanstack/react-table'
export type { ColumnDef } from '@tanstack/react-table'

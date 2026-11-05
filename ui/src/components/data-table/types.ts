import '@tanstack/react-table'

/** Tailwind container query sizes: sm is 24rem, 7xl is 80rem. */
export type ContainerSize =
  'sm' | 'md' | 'lg' | 'xl' | '2xl' | '3xl' | '4xl' | '5xl' | '6xl' | '7xl'

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
    /**
     * Smallest width of the TABLE (a container query, not the viewport) at which the column
     * shows. Below it the column is hidden, so a narrow table never scrolls sideways. Give it
     * to the columns a reader can do without; the detail page still shows their values.
     */
    showFrom?: ContainerSize
    /**
     * The column takes the free width and truncates its content instead of widening the
     * table. Use it for one long value per table, such as a URL.
     */
    fluid?: boolean
  }
}

export { createColumnHelper } from '@tanstack/react-table'
export type { ColumnDef } from '@tanstack/react-table'

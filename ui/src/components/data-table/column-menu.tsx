import type { Table } from '@tanstack/react-table'
import { Columns3 } from 'lucide-react'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { t } from '@/i18n'

export function columnLabel<T>(column: ReturnType<Table<T>['getAllLeafColumns']>[number]): string {
  const header = column.columnDef.header
  return column.columnDef.meta?.label ?? (typeof header === 'string' ? header : column.id)
}

/** Column chooser: a checkbox per hideable column. Pinned columns (meta.hideable false) are absent. */
export function ColumnMenu<T>({ table }: { table: Table<T> }) {
  const columns = table
    .getAllLeafColumns()
    .filter((c) => c.getCanHide() && c.columnDef.meta?.hideable !== false)
  if (columns.length === 0) return null
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="outline" size="sm" aria-label={t('table.columnsAria')}>
          <Columns3 aria-hidden="true" />
          {t('table.columns')}
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent>
        <DropdownMenuLabel>{t('table.columns')}</DropdownMenuLabel>
        {columns.map((column) => (
          <DropdownMenuCheckboxItem
            key={column.id}
            checked={column.getIsVisible()}
            // Keep the menu open: people toggle several columns in one go.
            onSelect={(event) => event.preventDefault()}
            onCheckedChange={(visible) => column.toggleVisibility(visible === true)}
          >
            {columnLabel(column)}
          </DropdownMenuCheckboxItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}

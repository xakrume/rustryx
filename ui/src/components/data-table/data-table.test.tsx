import { screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useState } from 'react'
import { describe, expect, it, vi } from 'vitest'
import { ApiError } from '@/api/errors'
import { STORAGE_KEYS } from '@/lib/constants'
import { expectNoA11yViolations, renderWithProviders } from '@/test/utils'
import { DataTable, type AnyColumnDef, type DataTableProps } from './data-table'
import { FilterBar, type FilterDef } from './filter-bar'

interface Row {
  id: string
  name: string
  size: number
}

const ROWS: Row[] = [
  { id: '1', name: 'maven-releases', size: 10 },
  { id: '2', name: 'npm-proxy', size: 20 },
  { id: '3', name: 'pypi-group', size: 30 },
]

const columns: AnyColumnDef<Row>[] = [
  {
    accessorKey: 'name',
    header: 'Name',
    meta: { label: 'Name', sortKey: 'name', hideable: false },
  },
  { accessorKey: 'size', header: 'Size', meta: { label: 'Size', align: 'right' } },
]

function setup(props: Partial<DataTableProps<Row>> = {}) {
  return renderWithProviders(
    <DataTable<Row>
      tableId="test"
      label="Repositories"
      columns={columns}
      rows={ROWS}
      getRowId={(r) => r.id}
      isLoading={false}
      {...props}
    />,
  )
}

describe('DataTable', () => {
  it('renders a labelled table with a header row and the data, accessibly', async () => {
    const { container } = setup()
    expect(screen.getByRole('region', { name: 'Repositories' })).toHaveAttribute('tabindex', '0')
    expect(screen.getByRole('table', { name: 'Repositories' })).toBeInTheDocument()
    expect(screen.getByRole('columnheader', { name: 'Name' })).toBeInTheDocument()
    expect(screen.getAllByRole('row')).toHaveLength(4)
    expect(screen.getByRole('cell', { name: 'npm-proxy' })).toBeInTheDocument()
    await expectNoA11yViolations(container)
  })

  it('makes the scroll region keyboard focusable for the horizontal scroll fallback', () => {
    setup()
    const region = screen.getByRole('region', { name: 'Repositories' })
    expect(region.className).toContain('overflow-auto')
    expect(region).toHaveAttribute('tabindex', '0')
  })

  describe('server side sort', () => {
    it('sends the API sort value and reports it with aria-sort', async () => {
      const user = userEvent.setup()
      const onSortChange = vi.fn()
      setup({ sort: 'name', onSortChange })
      const header = screen.getByRole('columnheader', { name: /Name/ })
      expect(header).toHaveAttribute('aria-sort', 'ascending')
      await user.click(within(header).getByRole('button'))
      expect(onSortChange).toHaveBeenCalledWith('-name')
    })

    it('shows descending and not sorted states, and only sortable columns are buttons', () => {
      setup({ sort: '-name', onSortChange: vi.fn() })
      expect(screen.getByRole('columnheader', { name: /Name/ })).toHaveAttribute(
        'aria-sort',
        'descending',
      )
      expect(
        within(screen.getByRole('columnheader', { name: 'Size' })).queryByRole('button'),
      ).toBeNull()
      expect(screen.getByRole('columnheader', { name: 'Size' })).not.toHaveAttribute('aria-sort')
    })

    it('starts ascending on a column that is not the current sort', async () => {
      const user = userEvent.setup()
      const onSortChange = vi.fn()
      setup({ sort: 'created_at', onSortChange })
      await user.click(
        within(screen.getByRole('columnheader', { name: /Name/ })).getByRole('button'),
      )
      expect(onSortChange).toHaveBeenCalledWith('name')
    })
  })

  describe('keyset paging', () => {
    it('offers load more while a next page exists, and says when everything is loaded', async () => {
      const user = userEvent.setup()
      const onLoadMore = vi.fn()
      const first = setup({ hasNextPage: true, onLoadMore })
      await user.click(screen.getByRole('button', { name: 'Load more' }))
      expect(onLoadMore).toHaveBeenCalledTimes(1)
      expect(screen.getByRole('status')).toHaveTextContent('3 rows')
      first.unmount()
      setup({ hasNextPage: false })
      expect(screen.queryByRole('button', { name: 'Load more' })).not.toBeInTheDocument()
      expect(screen.getByRole('status')).toHaveTextContent('All results loaded')
    })

    it('disables load more while loading and offers a page size', async () => {
      const user = userEvent.setup()
      const onPageSizeChange = vi.fn()
      setup({
        hasNextPage: true,
        onLoadMore: vi.fn(),
        isLoadingMore: true,
        pageSize: 50,
        onPageSizeChange,
      })
      expect(screen.getByRole('button', { name: 'Loading more' })).toBeDisabled()
      await user.selectOptions(screen.getByLabelText('Rows per page'), '100')
      expect(onPageSizeChange).toHaveBeenCalledWith(100)
    })
  })

  describe('row selection and the bulk action bar', () => {
    function Harness() {
      const [bulk, setBulk] = useState('')
      return (
        <DataTable<Row>
          tableId="sel"
          label="Repositories"
          columns={columns}
          rows={ROWS}
          getRowId={(r) => r.id}
          isLoading={false}
          selectable
          selectionLabel={(r) => `Select ${r.name}`}
          bulkActions={({ selectedRows }) => (
            <button
              type="button"
              onClick={() => setBulk(selectedRows.map((r) => r.name).join(','))}
            >
              Delete selected ({bulk || 'none run'})
            </button>
          )}
        />
      )
    }

    it('shows the bar only while rows are selected, with a count and the actions', async () => {
      const user = userEvent.setup()
      const { container } = renderWithProviders(<Harness />)
      expect(screen.queryByRole('region', { name: 'Bulk actions' })).not.toBeInTheDocument()
      await user.click(screen.getByRole('checkbox', { name: 'Select maven-releases' }))
      await user.click(screen.getByRole('checkbox', { name: 'Select pypi-group' }))
      const bar = screen.getByRole('region', { name: 'Bulk actions' })
      expect(bar).toHaveTextContent('2 selected')
      await user.click(within(bar).getByRole('button', { name: /Delete selected/ }))
      expect(
        within(bar).getByRole('button', { name: /maven-releases,pypi-group/ }),
      ).toBeInTheDocument()
      await expectNoA11yViolations(container)
      await user.click(within(bar).getByRole('button', { name: 'Clear selection' }))
      expect(screen.queryByRole('region', { name: 'Bulk actions' })).not.toBeInTheDocument()
    })

    it('selects all loaded rows with the header checkbox, with an indeterminate state in between', async () => {
      const user = userEvent.setup()
      renderWithProviders(<Harness />)
      const all = screen.getByRole('checkbox', { name: 'Select all loaded rows' })
      await user.click(screen.getByRole('checkbox', { name: 'Select npm-proxy' }))
      expect(all).toHaveAttribute('aria-checked', 'mixed')
      await user.click(all)
      expect(screen.getByRole('region', { name: 'Bulk actions' })).toHaveTextContent('3 selected')
    })

    it('must_not_keep_a_selection_across_a_filter_change', async () => {
      const user = userEvent.setup()
      function Keyed() {
        const [key, setKey] = useState('a')
        return (
          <>
            <button onClick={() => setKey('b')}>change filter</button>
            <DataTable<Row>
              tableId="reset"
              label="R"
              columns={columns}
              rows={ROWS}
              getRowId={(r) => r.id}
              isLoading={false}
              selectable
              selectionLabel={(r) => `Select ${r.name}`}
              resetSelectionKey={key}
            />
          </>
        )
      }
      renderWithProviders(<Keyed />)
      await user.click(screen.getByRole('checkbox', { name: 'Select npm-proxy' }))
      expect(screen.getByRole('region', { name: 'Bulk actions' })).toBeInTheDocument()
      await user.click(screen.getByRole('button', { name: 'change filter' }))
      expect(screen.queryByRole('region', { name: 'Bulk actions' })).not.toBeInTheDocument()
    })
  })

  describe('column visibility', () => {
    it('hides a column, remembers it per table, and never offers to hide a pinned column', async () => {
      const user = userEvent.setup()
      const first = setup({ tableId: 'persist' })
      await user.click(screen.getByRole('button', { name: 'Choose visible columns' }))
      expect(screen.queryByRole('menuitemcheckbox', { name: 'Name' })).not.toBeInTheDocument()
      await user.click(await screen.findByRole('menuitemcheckbox', { name: 'Size' }))
      expect(screen.queryByRole('columnheader', { name: 'Size' })).not.toBeInTheDocument()
      expect(
        JSON.parse(window.localStorage.getItem(STORAGE_KEYS.tableColumns('persist')) ?? '{}'),
      ).toEqual({ size: false })
      first.unmount()
      setup({ tableId: 'persist' })
      expect(screen.queryByRole('columnheader', { name: 'Size' })).not.toBeInTheDocument()
    })

    it('must_not_crash_on_garbage_in_storage', () => {
      window.localStorage.setItem(STORAGE_KEYS.tableColumns('garbage'), '{"size": "yes"}')
      setup({ tableId: 'garbage' })
      expect(screen.getByRole('columnheader', { name: 'Size' })).toBeInTheDocument()
    })
  })

  describe('states', () => {
    it('shows a skeleton only after 150 ms, and the busy state at once', async () => {
      setup({ rows: [], isLoading: true })
      expect(screen.getByRole('table')).toHaveAttribute('aria-busy', 'true')
      expect(document.querySelector('[class*="animate-pulse-soft"]')).toBeNull()
      await waitFor(() =>
        expect(document.querySelector('[class*="animate-pulse-soft"]')).not.toBeNull(),
      )
    })

    it('keeps the old rows visible while refreshing', () => {
      setup({ isLoading: true })
      expect(screen.getByRole('cell', { name: 'npm-proxy' })).toBeInTheDocument()
    })

    it('shows the first use empty state, and a different one for filters', async () => {
      const user = userEvent.setup()
      const onClearFilters = vi.fn()
      const first = setup({ rows: [], emptyState: <p>Create the first repository</p> })
      expect(screen.getByText('Create the first repository')).toBeInTheDocument()
      first.unmount()
      setup({ rows: [], hasActiveFilters: true, onClearFilters })
      expect(screen.getByText('No results for these filters')).toBeInTheDocument()
      await user.click(screen.getByRole('button', { name: 'Clear filters' }))
      expect(onClearFilters).toHaveBeenCalled()
    })

    it('shows the error with code, request id and retry when the first load fails', async () => {
      const user = userEvent.setup()
      const onRetry = vi.fn()
      setup({
        rows: [],
        error: new ApiError({
          code: 'store_unavailable',
          message: 'PostgreSQL is unreachable',
          status: 503,
          requestId: 'req-5',
        }),
        onRetry,
      })
      expect(screen.getByText('The list could not be loaded')).toBeInTheDocument()
      expect(screen.getByText('store_unavailable')).toBeInTheDocument()
      expect(screen.getByText('req-5')).toBeInTheDocument()
      await user.click(screen.getByRole('button', { name: 'Retry' }))
      expect(onRetry).toHaveBeenCalled()
    })

    it('must_not_hide_loaded_rows_when_a_later_request_fails', () => {
      setup({
        error: new ApiError({ code: 'internal_error', message: 'x', status: 500, requestId: 'r' }),
        onRetry: vi.fn(),
      })
      expect(screen.getByText('Showing earlier data while refreshing')).toBeInTheDocument()
      expect(screen.getByRole('cell', { name: 'npm-proxy' })).toBeInTheDocument()
    })
  })
})

describe('FilterBar', () => {
  const filters: FilterDef[] = [
    { id: 'q', type: 'search', label: 'Name contains' },
    {
      id: 'format',
      type: 'select',
      label: 'Format',
      options: [
        { value: 'npm', label: 'npm' },
        { value: 'maven2', label: 'maven2' },
      ],
    },
  ]

  it('debounces search text and reports the new value once', async () => {
    const user = userEvent.setup()
    const onChange = vi.fn()
    renderWithProviders(
      <FilterBar filters={filters} values={{}} onChange={onChange} onClear={vi.fn()} />,
    )
    await user.type(screen.getByRole('searchbox', { name: 'Name contains' }), 'mav')
    expect(onChange).not.toHaveBeenCalled()
    await waitFor(() => expect(onChange).toHaveBeenCalledWith('q', 'mav'))
    expect(onChange).toHaveBeenCalledTimes(1)
  })

  it('reports select changes at once and shows removable chips for active filters', async () => {
    const user = userEvent.setup()
    const onChange = vi.fn()
    const onClear = vi.fn()
    const { container } = renderWithProviders(
      <FilterBar
        filters={filters}
        values={{ q: 'mav', format: 'npm' }}
        onChange={onChange}
        onClear={onClear}
      />,
    )
    await user.selectOptions(screen.getByLabelText('Format'), 'maven2')
    expect(onChange).toHaveBeenCalledWith('format', 'maven2')
    const chips = screen.getByRole('list', { name: 'Active filters' })
    expect(within(chips).getByText('mav')).toBeInTheDocument()
    await user.click(within(chips).getByRole('button', { name: 'Remove filter Format' }))
    expect(onChange).toHaveBeenCalledWith('format', null)
    await user.click(within(chips).getByRole('button', { name: 'Clear filters' }))
    expect(onClear).toHaveBeenCalled()
    await expectNoA11yViolations(container)
  })

  it('follows the parent when it clears the value, without losing typed characters', async () => {
    const user = userEvent.setup()
    function Harness() {
      const [q, setQ] = useState('abc')
      return (
        <>
          <FilterBar
            filters={filters}
            values={{ q }}
            onChange={(_, v) => setQ(v ?? '')}
            onClear={() => setQ('')}
          />
          <button onClick={() => setQ('')}>external clear</button>
        </>
      )
    }
    renderWithProviders(<Harness />)
    const box = screen.getByRole('searchbox', { name: 'Name contains' })
    expect(box).toHaveValue('abc')
    await user.click(screen.getByRole('button', { name: 'external clear' }))
    await waitFor(() => expect(box).toHaveValue(''))
  })
})

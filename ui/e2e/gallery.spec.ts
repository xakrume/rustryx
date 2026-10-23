import { expect, signIn, test } from './fixtures'

test.describe('component gallery against the mock API', () => {
  test.beforeEach(async ({ page }) => {
    await signIn(page, 'admin', '/__components')
    await expect(page.getByRole('cell', { name: 'maven-releases', exact: true })).toBeVisible()
  })

  test('sorts on the server and keeps the sort in the address bar', async ({ page }) => {
    const table = page.getByRole('table', { name: 'Repositories' })
    await table.getByRole('button', { name: /Sort by Name/ }).click()
    await expect(page).toHaveURL(/sort=-name/)
    await expect(table.getByRole('columnheader', { name: /Name/ })).toHaveAttribute(
      'aria-sort',
      'descending',
    )
    await expect(table.getByRole('row').nth(1)).toContainText('team-100')
  })

  test('filters with debounce, shows chips and clears them', async ({ page }) => {
    await page.getByRole('searchbox', { name: 'Name contains' }).fill('npm-')
    await expect(page).toHaveURL(/q=npm-/)
    await expect(page.getByRole('list', { name: 'Active filters' })).toContainText('npm-')
    await expect(page.getByRole('cell', { name: 'npm-proxy', exact: true })).toBeVisible()
    await expect(page.getByRole('cell', { name: 'maven-releases', exact: true })).toHaveCount(0)
    await page.getByRole('button', { name: 'Clear filters' }).click()
    await expect(page).not.toHaveURL(/q=npm-/)
    await expect(page.getByRole('cell', { name: 'maven-releases', exact: true })).toBeVisible()
  })

  test('shows the filtered empty state', async ({ page }) => {
    await page.getByRole('searchbox', { name: 'Name contains' }).fill('no-such-repository')
    await expect(page.getByText('No results for these filters')).toBeVisible()
  })

  test('loads more with the keyset token', async ({ page }) => {
    const rows = page.getByRole('table', { name: 'Repositories' }).getByRole('row')
    await expect(rows).toHaveCount(51)
    await page.getByRole('button', { name: 'Load more' }).click()
    await expect(rows).toHaveCount(101)
  })

  test('selects rows and shows the bulk action bar', async ({ page }) => {
    await page.getByRole('checkbox', { name: 'Select maven-releases' }).check()
    await page.getByRole('checkbox', { name: 'Select npm-proxy' }).check()
    const bar = page.getByRole('region', { name: 'Bulk actions' })
    await expect(bar).toContainText('2 selected')
    await bar.getByRole('button', { name: 'Clear selection' }).click()
    await expect(bar).toHaveCount(0)
  })

  test('hides a column and remembers the choice', async ({ page }) => {
    await page.getByRole('button', { name: 'Choose visible columns' }).click()
    await page.getByRole('menuitemcheckbox', { name: 'Size' }).click()
    await page.keyboard.press('Escape')
    await expect(page.getByRole('columnheader', { name: 'Size' })).toHaveCount(0)
    await page.reload()
    await expect(page.getByRole('columnheader', { name: 'Name' })).toBeVisible()
    await expect(page.getByRole('columnheader', { name: 'Size' })).toHaveCount(0)
  })

  test('deletes only after the exact name is typed', async ({ page }) => {
    await page.getByRole('button', { name: 'Actions for maven-releases' }).click()
    await page.getByRole('menuitem', { name: 'Delete' }).click()
    const dialog = page.getByRole('alertdialog')
    await expect(dialog).toContainText('maven-releases')
    const confirm = dialog.getByRole('button', { name: 'Delete repository' })
    await expect(confirm).toBeDisabled()
    await dialog.getByRole('textbox', { name: 'Confirmation text' }).fill('maven-releases')
    await expect(confirm).toBeEnabled()
    await confirm.click()
    await expect(page.getByText('Deleted (demo only)', { exact: true })).toBeVisible()
  })

  test('shows a secret once, only closable after the acknowledgement', async ({ page }) => {
    await page.getByRole('button', { name: 'Show a secret once' }).click()
    const dialog = page.getByRole('dialog', { name: /Copy your API token/ })
    await expect(dialog).toContainText('ryx_')
    await page.keyboard.press('Escape')
    await expect(dialog).toBeVisible()
    await expect(dialog.getByRole('button', { name: 'Done' })).toBeDisabled()
    await dialog.getByRole('checkbox', { name: 'I stored it in a safe place' }).check()
    await dialog.getByRole('button', { name: 'Done' }).click()
    await expect(dialog).toHaveCount(0)
  })

  test('toasts an API failure with the code and request id', async ({ page }) => {
    await page.getByRole('button', { name: 'API error toast' }).click()
    const region = page.getByRole('region', { name: 'Notifications' })
    await expect(region).toContainText('Could not delete the role')
    await expect(region).toContainText('internal_error')
    await expect(region.getByRole('button', { name: 'Copy Request ID' })).toBeVisible()
  })

  test('the tree opens lazily and survives a failed load', async ({ page }) => {
    const tree = page.getByRole('tree', { name: 'Repository content' })
    await tree.getByRole('treeitem', { name: 'com' }).focus()
    await page.keyboard.press('ArrowRight')
    await expect(tree.getByRole('treeitem', { name: 'example' })).toBeVisible()
    await tree.getByRole('treeitem', { name: /flaky/ }).click()
    await expect(tree.getByText(/Could not load/)).toBeVisible()
    await page.keyboard.press('ArrowLeft')
    await page.keyboard.press('ArrowRight')
    await expect(
      tree
        .getByRole('treeitem', { name: 'one' })
        .or(tree.getByRole('treeitem', { name: 'pom.xml' }))
        .first(),
    ).toBeVisible()
  })

  test('the chart has a data table behind it', async ({ page }) => {
    await page.getByRole('button', { name: 'Show data table' }).click()
    await expect(
      page.getByRole('table', { name: 'Storage used by blob store: data table' }),
    ).toBeVisible()
    await page.getByRole('button', { name: 'Show chart' }).click()
    await expect(page.locator('svg.recharts-surface')).toBeVisible()
  })

  test('submits a form and maps a server field error', async ({ page }) => {
    const form = page.getByRole('form', { name: 'Create repository' })
    await form.getByLabel(/^Name/).fill('taken')
    await form.getByLabel(/^Remote URL/).fill('https://registry.npmjs.org')
    await form.getByRole('button', { name: 'Create repository' }).click()
    await expect(form.getByText('This name is already used.')).toBeVisible()
    await expect(form.getByLabel(/^Name/)).toBeFocused()
  })
})

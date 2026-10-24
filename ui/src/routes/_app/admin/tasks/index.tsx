import { createFileRoute } from '@tanstack/react-router'
import { text } from '@/features/system/search-params'
import { TasksPage } from '@/features/tasks/tasks-page'

interface TasksSearch {
  /** `lifecycle` shows the run history tab. */
  view?: string
  /** A task id: the detail view replaces the list. */
  task?: string
  kind?: string
  scope?: string
  enabled?: string
  /** Lifecycle runs: `cleanup` or `gc`, the open run, and the history filters. */
  source?: string
  run?: string
  mode?: string
  status?: string
}

export const Route = createFileRoute('/_app/admin/tasks/')({
  validateSearch: (search: Record<string, unknown>): TasksSearch => ({
    view: text(search.view),
    task: text(search.task),
    kind: text(search.kind),
    scope: text(search.scope),
    enabled: text(search.enabled),
    source: text(search.source),
    run: text(search.run),
    mode: text(search.mode),
    status: text(search.status),
  }),
  component: TasksPage,
})

import { createFileRoute } from '@tanstack/react-router'
import { TaskRunPage } from '@/features/tasks/task-run-page'

export const Route = createFileRoute('/_app/admin/tasks/$id/runs/$run')({
  staticData: { crumb: 'pages.taskRun' },
  component: function TaskRunRoute() {
    const { id, run } = Route.useParams()
    return <TaskRunPage taskId={id} runId={run} />
  },
})

import { createFileRoute } from '@tanstack/react-router'
import { UserDetailPage } from '@/features/users/user-detail-page'

export const Route = createFileRoute('/_app/admin/users/$id')({
  staticData: { crumb: 'pages.userDetail' },
  component: function UserDetailRoute() {
    return <UserDetailPage id={Route.useParams().id} />
  },
})

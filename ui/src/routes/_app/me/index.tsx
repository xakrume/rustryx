import { createFileRoute } from '@tanstack/react-router'
import { ProfilePage } from '@/features/profile/profile-page'

export const Route = createFileRoute('/_app/me/')({
  staticData: { crumb: 'nav.items.profile' },
  component: ProfilePage,
})

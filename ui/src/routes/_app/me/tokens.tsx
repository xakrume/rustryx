import { createFileRoute } from '@tanstack/react-router'
import { pickText } from '@/features/roles/search-params'
import { MyTokensPage } from '@/features/tokens/my-tokens-page'

export const Route = createFileRoute('/_app/me/tokens')({
  staticData: { crumb: 'nav.items.personalTokens' },
  validateSearch: (search: Record<string, unknown>) => pickText(search, ['state', 'new'] as const),
  component: MyTokensPage,
})

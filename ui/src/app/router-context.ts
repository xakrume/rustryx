import type { QueryClient } from '@tanstack/react-query'

/** Available to every route's beforeLoad and loader as `context`. */
export interface RouterContext {
  queryClient: QueryClient
}

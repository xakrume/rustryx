import { QueryClientProvider } from '@tanstack/react-query'
import { RouterProvider } from '@tanstack/react-router'
import { StrictMode, useState } from 'react'
import { createQueryClient } from '@/api/query-client'
import { ErrorBoundary } from '@/components/error-boundary'
import { Toaster } from '@/components/toaster'
import { ThemeProvider } from '@/features/theme/theme-provider'
import { toast } from '@/lib/toast-store'
import { createAppRouter } from './router'

export function App() {
  // Created once per mount: state, not module level, so tests and HMR get a fresh cache.
  const [queryClient] = useState(() =>
    createQueryClient({ onMutationError: (error) => toast.apiError(error) }),
  )
  const [router] = useState(() => createAppRouter({ queryClient }))
  return (
    <StrictMode>
      <ErrorBoundary>
        <QueryClientProvider client={queryClient}>
          <ThemeProvider>
            {/* Toaster stays here so renderApp tests and every route (including login) see it.
                TooltipProvider lives in AppShell so floating-ui stays out of the entry. */}
            <RouterProvider router={router} />
            <Toaster />
          </ThemeProvider>
        </QueryClientProvider>
      </ErrorBoundary>
    </StrictMode>
  )
}

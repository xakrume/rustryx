import { QueryClientProvider } from '@tanstack/react-query'
import { RouterProvider } from '@tanstack/react-router'
import { StrictMode, useState } from 'react'
import { createQueryClient } from '@/api/query-client'
import { ErrorBoundary } from '@/components/error-boundary'
import { Toaster } from '@/components/toaster'
import { TooltipProvider } from '@/components/ui/tooltip'
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
            <TooltipProvider delayDuration={300}>
              <RouterProvider router={router} />
              <Toaster />
            </TooltipProvider>
          </ThemeProvider>
        </QueryClientProvider>
      </ErrorBoundary>
    </StrictMode>
  )
}

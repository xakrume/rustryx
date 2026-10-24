import { useQuery } from '@tanstack/react-query'
import { useDebouncedValue } from '@/hooks/use-debounced-value'
import { selectorKeys, validateExpression, type CompileIssue } from './api'

export const VALIDATE_DELAY_MS = 400

export interface LiveValidation {
  status: 'idle' | 'checking' | 'valid' | 'invalid' | 'failed'
  errors: readonly CompileIssue[]
  /** The expression the result is about; it differs from the text box while the user types. */
  checked: string
  error: unknown
}

/**
 * Compiles the expression on the server while the user types (`POST /content-selectors/validate`),
 * after a short pause. The call never changes anything and a syntax error is a RESULT (always
 * 200), so a bad expression shows its errors with positions without any save. The result of an
 * older text is never shown for a newer one: while the two differ the status is `checking`.
 */
export function useLiveValidation(expression: string): LiveValidation {
  const debounced = useDebouncedValue(expression, VALIDATE_DELAY_MS)
  const active = debounced.trim() !== ''
  const query = useQuery({
    queryKey: [...selectorKeys.all, 'validate', debounced],
    queryFn: ({ signal }) => validateExpression(debounced, signal),
    enabled: active,
    staleTime: Infinity,
    gcTime: 60_000,
    retry: false,
  })
  if (expression.trim() === '') return { status: 'idle', errors: [], checked: '', error: null }
  if (expression !== debounced || query.isPending) {
    return { status: 'checking', errors: [], checked: debounced, error: null }
  }
  if (query.isError) return { status: 'failed', errors: [], checked: debounced, error: query.error }
  return {
    status: query.data.valid ? 'valid' : 'invalid',
    errors: query.data.errors,
    checked: debounced,
    error: null,
  }
}

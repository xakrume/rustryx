import { isApiError } from '@/api/errors'

/**
 * A page's own title for a failed load ("The repository could not be loaded"), except for 404:
 * there the server answers the same for "does not exist" and "hidden from you", and the shared
 * panel says exactly that, which a page specific title would hide (docs/ui-page-inventory.md, 5).
 */
export function failureTitle(error: unknown, title: string): string | undefined {
  return isApiError(error) && error.status === 404 ? undefined : title
}

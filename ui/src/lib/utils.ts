import { clsx, type ClassValue } from 'clsx'

/**
 * Join class names. We keep this as `clsx` only: `tailwind-merge` is ~18 KiB gzip and
 * lands in the initial bundle through every `cn()` call. Component APIs pass complete
 * utility sets rather than conflicting overrides, so merge is not needed for correctness.
 */
export function cn(...inputs: ClassValue[]): string {
  return clsx(inputs)
}

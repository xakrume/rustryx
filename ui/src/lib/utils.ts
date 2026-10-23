import { clsx, type ClassValue } from 'clsx'
import { twMerge } from 'tailwind-merge'

/** Join class names and resolve Tailwind conflicts (the last utility wins). */
export function cn(...inputs: ClassValue[]): string {
  return twMerge(clsx(inputs))
}

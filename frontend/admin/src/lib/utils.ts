import { type ClassValue, clsx } from 'clsx'
import { twMerge } from 'tailwind-merge'

/**
 * Joins class names, with later Tailwind utilities winning over earlier ones.
 *
 * `clsx` alone would leave `"p-2 p-4"` in the output, and which of the two the
 * browser applies would come down to stylesheet order rather than to the order
 * they were written. `twMerge` resolves that at the point the string is built,
 * which is what makes a component's prop able to override its own default.
 */
export function cn(...inputs: ClassValue[]) {
  return twMerge(clsx(inputs))
}

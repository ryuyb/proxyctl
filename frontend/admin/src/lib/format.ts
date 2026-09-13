/**
 * Formatting for the values this interface renders.
 *
 * # Why these are functions rather than a formatter library
 *
 * Every one of them has a decision in it that a library would make differently.
 * Byte counts from this agent run to terabytes and are never negative; timestamps
 * arrive as Unix seconds and are frequently absent; a delay is a number that means
 * "not measured" as often as it means a value. Each is a small rule the interface
 * depends on, and stating it here is cheaper than configuring a general library to
 * agree.
 */

import type { Language } from './i18n'
import { localeTag } from './i18n'

/**
 * A byte count, in the largest unit that keeps it readable.
 *
 * Binary units (1024), matching what the kernel reports. Decimal units would
 * disagree with every other number an operator sees for the same transfer.
 */
export function bytes(value: number): string {
  if (!Number.isFinite(value) || value < 0) return '—'
  const units = ['B', 'KB', 'MB', 'GB', 'TB', 'PB']
  let amount = value
  let unit = 0
  while (amount >= 1024 && unit < units.length - 1) {
    amount /= 1024
    unit += 1
  }
  // One decimal below 10 of a unit, none above: "1.5 MB" is useful and "1,024.0 KB"
  // is not, so the precision follows the magnitude rather than being fixed.
  const decimals = unit === 0 ? 0 : amount < 10 ? 1 : 0
  return `${amount.toFixed(decimals)} ${units[unit]}`
}

/** A rate, in bytes per second. */
export function rate(value: number): string {
  return `${bytes(value)}/s`
}

/**
 * A duration in seconds, as a compact phrase.
 *
 * Days and hours are the useful granularities for a session or a subscription
 * interval; rendering "259200 seconds" would be technically correct and useless.
 */
export function duration(
  seconds: number,
  t: (key: string, options?: Record<string, unknown>) => string,
): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '—'
  if (seconds < 60) return t('time.inSeconds', { count: Math.round(seconds) })
  if (seconds < 3600) return t('time.inMinutes', { count: Math.round(seconds / 60) })
  if (seconds < 86400) return `${Math.round(seconds / 3600)}h`
  return `${Math.round(seconds / 86400)}d`
}

/**
 * A past instant, as a relative phrase.
 *
 * `HH:MM` within the last hour is skipped deliberately: this interface is usually
 * read while something is happening, and "3m ago" answers "is this current" in a
 * way a clock time does not.
 */
export function relative(
  unixSeconds: number | null | undefined,
  t: (key: string, options?: Record<string, unknown>) => string,
): string {
  if (unixSeconds == null) return t('common.never')
  const delta = Date.now() / 1000 - unixSeconds
  // A future timestamp is possible: the agent's clock and the browser's are
  // different clocks. It is reported as "just now" rather than as a negative age,
  // which would look like a defect in this interface rather than clock skew.
  if (delta < 5) return t('time.justNow')
  if (delta < 3600) return t('time.secondsAgo', { count: Math.round(delta / 60) * 60 || Math.round(delta) })
  if (delta < 86400) return t('time.hoursAgo', { count: Math.round(delta / 3600) })
  return t('time.daysAgo', { count: Math.round(delta / 86400) })
}

/**
 * An exact instant, as a local date and time.
 *
 * Shown alongside a relative phrase where the precise moment matters — an audit
 * entry, a config activation — because "2d ago" cannot be compared against an
 * external log.
 */
export function timestamp(unixSeconds: number | null | undefined, language: Language): string {
  if (unixSeconds == null) return '—'
  return new Date(unixSeconds * 1000).toLocaleString(localeTag(language), {
    year: 'numeric',
    month: '2-digit',
    day: '2-digit',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

/** A time of day only. */
export function clock(unixSeconds: number | null | undefined, language: Language): string {
  if (unixSeconds == null) return '—'
  return new Date(unixSeconds * 1000).toLocaleTimeString(localeTag(language), {
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  })
}

/** A millisecond delay, or a marker that none was measured. */
export function delay(millis: number | null | undefined, t: (key: string, options?: Record<string, unknown>) => string): string {
  if (millis == null) return t('mihomo.noDelay')
  return t('mihomo.delayValue', { ms: millis })
}

/**
 * Shortens a checksum for display.
 *
 * The full value is what an operator compares against a file, so it is available
 * on hover rather than being the thing that pushes a column out of view.
 */
export function shortChecksum(checksum: string): string {
  const separator = checksum.indexOf(':')
  const digest = separator >= 0 ? checksum.slice(separator + 1) : checksum
  return digest.length > 12 ? digest.slice(0, 12) : digest
}

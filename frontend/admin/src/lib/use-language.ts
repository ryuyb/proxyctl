/**
 * The current language, as the union the formatters expect.
 *
 * # Why this exists rather than reading `i18n.language` at each call site
 *
 * `i18n.language` is a `string`, and it is not always one of the offered values —
 * during initialisation it can be the empty string, and a region-tagged value
 * arrives as `zh-CN`. Every call site would otherwise repeat the same
 * `startsWith('zh')` normalisation, and the one that forgot would be the one that
 * formats a date in the wrong locale.
 */

import { useTranslation } from 'react-i18next'

import type { Language } from './i18n'

/** The active language, normalised to an offered value. */
export function useLanguage(): Language {
  const { i18n } = useTranslation()
  return i18n.language?.startsWith('zh') ? 'zh' : 'en'
}

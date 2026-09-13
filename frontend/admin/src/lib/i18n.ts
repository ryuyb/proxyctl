/**
 * The interface's translations.
 *
 * # Why every user-facing string goes through here
 *
 * The agent's own messages are in English and are not translated: they are
 * protocol, and a Chinese-language machine and an English-language one should
 * produce the same failure text for the same failure. What is translated is the
 * interface's *own* vocabulary — the words this bundle chose — plus the labels for
 * the agent's stable identifiers, which are a closed set the agent controls.
 *
 * # Keys are addressed by meaning, not by English
 *
 * `nav.connections`, not `连接`. A key that happens to be its English text makes
 * the English locale look finished while hiding every string that was never
 * extracted, and renaming a label would then rename a key.
 *
 * # Parity is enforced by a test
 *
 * `lib/i18n.test.ts` fails when the two locales disagree about a key, in either
 * direction. A missing translation that silently falls back to the key name is the
 * one failure mode that reaches production, because it looks like a rendering bug
 * rather than a missing string.
 */

import i18n from 'i18next'
import { initReactI18next } from 'react-i18next'

import { en } from './locales/en'
import { zh } from './locales/zh'

/** The languages this interface offers. */
export const LANGUAGES = ['en', 'zh'] as const

/** One of the offered languages. */
export type Language = (typeof LANGUAGES)[number]

/** Where the chosen language is remembered between visits. */
const STORAGE_KEY = 'proxyctl.language'

/** The language to start in. */
export function initialLanguage(): Language {
  // An explicit choice always wins, because a browser's `Accept-Language` is about
  // the reader's habits and this is about their decision.
  const stored = readStored()
  if (stored) return stored

  // Otherwise follow the browser. Only a `zh` prefix maps to Chinese: a reader of
  // `zh-TW` or `zh-Hans` is served Chinese rather than English, and every other
  // language is served English rather than a machine translation of it.
  const preferred = navigator.language?.toLowerCase() ?? ''
  return preferred.startsWith('zh') ? 'zh' : 'en'
}

/** Reads the remembered language, validating rather than trusting it. */
function readStored(): Language | null {
  try {
    const value = localStorage.getItem(STORAGE_KEY)
    // Validated against the offered set: `localStorage` is shared with every
    // script on the origin and survives upgrades, so a value from an older build
    // or another writer must not be able to put i18next into an unknown language.
    return LANGUAGES.includes(value as Language) ? (value as Language) : null
  } catch {
    // Storage can be unavailable — a privacy mode, a disabled cookie policy. That
    // is not a reason to fail to start.
    return null
  }
}

/** Remembers the chosen language. */
export function rememberLanguage(language: Language): void {
  try {
    localStorage.setItem(STORAGE_KEY, language)
  } catch {
    // See `readStored`. A session that forgets the choice is usable; one that
    // refuses to start is not.
  }
}

/** The BCP-47 tag for a language, used for `lang` and for date formatting. */
export function localeTag(language: Language): string {
  return language === 'zh' ? 'zh-CN' : 'en-US'
}

/**
 * Keeps `<html lang>` in step with the active language.
 *
 * # Why this is an i18next event and not a line in the toggle
 *
 * The attribute has to be right in two cases, and only one of them is a click.
 * The other is a page load where the stored language is applied before the first
 * render — and a click handler cannot run then. Setting it only on click left a
 * reloaded Chinese interface announcing itself as English, which is what a screen
 * reader reads the text as.
 *
 * i18next fires `languageChanged` for both, so subscribing covers the two without
 * anyone having to remember the second.
 */
function followDocumentLanguage(language: string): void {
  document.documentElement.lang = localeTag(language.startsWith('zh') ? 'zh' : 'en')
}

await i18n.use(initReactI18next).init({
  resources: {
    en: { translation: en },
    zh: { translation: zh },
  },
  lng: initialLanguage(),
  // A key with no translation renders as the key itself, which makes the gap
  // visible in the running interface instead of rendering an empty element.
  // The parity test is what catches it before that point.
  fallbackLng: 'en',
  interpolation: {
    // React escapes interpolated values already. Leaving i18next's own escaping on
    // would double-encode an ampersand in a subscription name.
    escapeValue: false,
  },
  returnNull: false,
})

// Applied once for the initial language, and again on every change. The initial
// call matters: a reload that restores a stored language never fires a click.
followDocumentLanguage(i18n.language)
i18n.on('languageChanged', followDocumentLanguage)

export default i18n

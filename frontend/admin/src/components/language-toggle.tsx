/**
 * The language control.
 *
 * # Why this is a two-state button rather than a dropdown
 *
 * There are two languages. A menu for two options costs a click to open and a
 * click to choose, and shows the options only after the first click; a button
 * labelled with the *other* language is one click and is always visible. If a
 * third language is added this should become a menu, not grow a third state.
 */

import { useTranslation } from 'react-i18next'
import { Languages } from 'lucide-react'

import { Button } from '@/components/ui/button'
import { rememberLanguage, type Language } from '@/lib/i18n'

export function LanguageToggle() {
  const { i18n, t } = useTranslation()
  const current: Language = i18n.language.startsWith('zh') ? 'zh' : 'en'
  const next: Language = current === 'zh' ? 'en' : 'zh'

  const switchTo = (language: Language) => {
    void i18n.changeLanguage(language)
    // `<html lang>` follows from `languageChanged`, which `lib/i18n.ts`
    // subscribes to. Setting it here as well would be a second place that has to
    // stay correct, and the one that was here already failed to cover a reload.
    rememberLanguage(language)
  }

  return (
    <Button
      variant="outline"
      size="sm"
      className="w-full justify-start gap-2"
      onClick={() => switchTo(next)}
      title={`${t('language.label')}: ${t(`language.${current}`)}`}
    >
      <Languages className="size-3.5" />
      <span className="text-xs">{t(`language.${next}`)}</span>
    </Button>
  )
}

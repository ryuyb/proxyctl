/**
 * The locale parity check.
 *
 * # Why this is a test and not a type
 *
 * `zh.ts` is typed as `Translation`, so a *missing* key is a compile error. That
 * covers one direction and misses two others:
 *
 * * An **extra** key in `zh` — a translation left behind after its English key was
 *   renamed — is structurally fine, and ships as a string nothing can ever render.
 * * A **placeholder mismatch** is invisible to the type system entirely. `{{count}}`
 *   in English and `{{conut}}` in Chinese both have type `string`, and the Chinese
 *   sentence renders with a literal `{{conut}}` in the middle of it.
 *
 * Both are the kind of defect that reaches a user, so both are asserted here.
 */

import { describe, expect, it } from 'vitest'

import { en } from './locales/en'
import { zh } from './locales/zh'

/** Every leaf path in a nested object, as `a.b.c`. */
function leafPaths(value: unknown, prefix = ''): string[] {
  if (typeof value !== 'object' || value === null) return [prefix]
  return Object.entries(value as Record<string, unknown>).flatMap(([key, child]) =>
    leafPaths(child, prefix ? `${prefix}.${key}` : key),
  )
}

/** Reads a path out of a nested object. */
function at(root: unknown, path: string): unknown {
  return path.split('.').reduce<unknown>((node, key) => {
    if (typeof node !== 'object' || node === null) return undefined
    return (node as Record<string, unknown>)[key]
  }, root)
}

/** The `{{name}}` placeholders in a string, as a sorted set. */
function placeholders(text: string): string[] {
  return [...text.matchAll(/\{\{\s*([a-zA-Z0-9_]+)\s*\}\}/g)]
    .map((match) => match[1])
    .sort()
}

const english = leafPaths(en).sort()
const chinese = leafPaths(zh).sort()

describe('locales', () => {
  /**
   * The two locales must describe the same set of strings.
   *
   * Asserted in both directions: a key only in English is an untranslated string,
   * and one only in Chinese is a leftover.
   */
  it('define exactly the same keys', () => {
    const missing = english.filter((path) => !chinese.includes(path))
    const extra = chinese.filter((path) => !english.includes(path))
    expect(missing, 'keys missing from zh').toEqual([])
    expect(extra, 'keys in zh that en does not have').toEqual([])
  })

  /**
   * A placeholder must survive translation with the same name.
   *
   * `{{count}}` becoming `{{conut}}` renders the braces verbatim, and the sentence
   * still looks nearly right — which is why it survives review.
   */
  it('agree on every placeholder', () => {
    const mismatched: string[] = []
    for (const path of english) {
      const source = at(en, path)
      const target = at(zh, path)
      if (typeof source !== 'string' || typeof target !== 'string') continue
      if (placeholders(source).join() !== placeholders(target).join()) {
        mismatched.push(
          `${path}: en has [${placeholders(source)}], zh has [${placeholders(target)}]`,
        )
      }
    }
    expect(mismatched).toEqual([])
  })

  /**
   * No string is empty or left as a placeholder.
   *
   * A blank translation renders an empty element, which reads as a layout bug
   * rather than a missing string, and is the hardest kind to report.
   */
  it('have no empty or whitespace-only values', () => {
    const blank: string[] = []
    for (const [locale, tree] of [
      ['en', en],
      ['zh', zh],
    ] as const) {
      for (const path of leafPaths(tree)) {
        const value = at(tree, path)
        if (typeof value === 'string' && value.trim() === '') blank.push(`${locale}:${path}`)
      }
    }
    expect(blank).toEqual([])
  })

  /**
   * The Chinese locale is genuinely translated, not copied.
   *
   * A locale file that was duplicated from English and never edited type-checks
   * perfectly and produces an interface that claims to offer Chinese while showing
   * English. This asserts that the two differ in a substantial fraction of their
   * strings, which a copy-paste cannot satisfy.
   */
  it('is actually translated, not copied from English', () => {
    const shared = english.filter((path) => {
      const source = at(en, path)
      return typeof source === 'string' && source === at(zh, path)
    })
    // A handful of leaves are legitimately identical — product names, `English`,
    // `Proxy Control`, `UID`, `KB` — so the check is on the proportion, not on
    // there being none.
    expect(shared.length).toBeLessThan(english.length * 0.15)
  })
})

/**
 * Guards on the generated UI components.
 *
 * # Why a test rather than trusting the generator
 *
 * `shadcn add` resolves the `@/` alias from `tsconfig.json`'s `paths`. When it
 * cannot — because the alias lives in a referenced project, or the root config is
 * shaped in a way it does not expect — it does not fail. It writes an import of
 * the *literal* remainder of the alias, so `@/lib/utils` becomes `import { cn }
 * from "cn"`, and npm resolves `cn` to an unrelated package that happens to exist.
 *
 * The result compiles, lints, and renders wrong. It was found twice by hand, which
 * is exactly once more than it should have been.
 */

import { readdirSync, readFileSync } from 'node:fs'
import { join } from 'node:path'

import { describe, expect, it } from 'vitest'

const UI_DIR = join(import.meta.dirname, '..', 'components', 'ui')

/** Every generated component's source. */
function components(): Array<{ name: string; source: string }> {
  return readdirSync(UI_DIR)
    .filter((name) => name.endsWith('.tsx'))
    .map((name) => ({ name, source: readFileSync(join(UI_DIR, name), 'utf8') }))
}

describe('generated ui components', () => {
  it('includes the components the application uses', () => {
    const names = components().map((component) => component.name)
    for (const expected of [
      'alert.tsx',
      'badge.tsx',
      'button.tsx',
      'card.tsx',
      'checkbox.tsx',
      'dialog.tsx',
      'input.tsx',
      'label.tsx',
      'select.tsx',
      'skeleton.tsx',
      'table.tsx',
      'tabs.tsx',
      'textarea.tsx',
      'tooltip.tsx',
    ]) {
      expect(names).toContain(expected)
    }
  })

  /**
   * No component may import a bare `cn`.
   *
   * That specifier is what the generator produces when it fails to resolve the
   * alias, and `cn` is a real package on the registry — so the import succeeds and
   * the class names are wrong rather than missing.
   */
  it('import every `cn` from the aliased util, never a bare package', () => {
    const offenders = components()
      .filter((component) => /from\s+["']cn["']/.test(component.source))
      .map((component) => component.name)
    expect(offenders, 'run `sed -i "" \'s|from "cn"|from "@/lib/utils"|\' src/components/ui/*.tsx`').toEqual([])
  })

  /**
   * The alias must be *used*, not merely permitted. A component that stopped
   * importing `cn` at all would pass the check above while having been rewritten
   * by something.
   */
  it('resolve every `@/` import to a file that exists', () => {
    const unresolved: string[] = []
    for (const { name, source } of components()) {
      for (const match of source.matchAll(/from\s+["'](@\/[^"']+)["']/g)) {
        const [path] = [match[1].replace(/^@\//, '')]
        const candidates = [
          join(import.meta.dirname, '..', `${path}.ts`),
          join(import.meta.dirname, '..', `${path}.tsx`),
          join(import.meta.dirname, '..', path, 'index.ts'),
        ]
        if (!candidates.some((candidate) => isFile(candidate))) {
          unresolved.push(`${name}: ${match[1]}`)
        }
      }
    }
    expect(unresolved).toEqual([])
  })
})

/** Whether a path names a readable file. */
function isFile(path: string): boolean {
  try {
    readFileSync(path)
    return true
  } catch {
    return false
  }
}

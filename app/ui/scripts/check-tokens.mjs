// Fails when a colour is written anywhere under src/ except styles/tokens.css. Components reference tokens only.
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, relative } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = join(fileURLToPath(new URL('.', import.meta.url)), '..', 'src')
const allowed = join(root, 'styles', 'tokens.css')
const patterns = [
  [/#[0-9a-fA-F]{3,8}\b/g, 'hex colour'],
  [/\b(?:rgb|rgba|hsl|hsla|hwb|lab|lch|oklab|oklch)\(/g, 'colour function'],
]

function* walk(dir) {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name)
    if (statSync(p).isDirectory()) yield* walk(p)
    else if (/\.(css|ts|tsx)$/.test(name)) yield p
  }
}

let bad = 0
for (const file of walk(root)) {
  if (file === allowed) continue
  const lines = readFileSync(file, 'utf8').split('\n')
  lines.forEach((line, i) => {
    // Strip comments so a colour mentioned in prose does not trip the check.
    const code = line.replace(/\/\*.*?\*\//g, '').replace(/(^|\s)\/\/.*$/, '')
    for (const [re, what] of patterns) {
      re.lastIndex = 0
      for (const m of code.matchAll(re)) {
        console.error(`${relative(process.cwd(), file)}:${i + 1}: ${what} "${m[0]}" outside tokens.css`)
        bad++
      }
    }
  })
}
if (bad) {
  console.error(`\n${bad} hard-coded colour(s). Move them into src/styles/tokens.css as tokens.`)
  process.exit(1)
}
console.log('check:tokens ok')

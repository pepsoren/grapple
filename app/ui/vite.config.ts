import react from '@vitejs/plugin-react'
import { existsSync, createReadStream } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'
import { defineConfig, type Plugin } from 'vite'

const here = dirname(fileURLToPath(import.meta.url))
const assets = resolve(here, '../../mockups/assets')

// Dev only: serves /@dev-art/<id>/<kind> from mockups/assets, the same mapping as the Rust mock backend,
// so the UI can be developed in a plain browser (src/lib/devbackend.ts).
function devArt(): Plugin {
  const pick = (id: string, kind: string): string | null => {
    const sid = decodeURIComponent(id).split(':')[1] ?? ''
    const c: Record<string, string[]> = {
      cover: [`${sid}-cover.jpg`, `covers/${sid}.jpg`, `dl/${sid}-tall.jpg`],
      hero: [sid === 'sugar' ? 'sugar-hero-clean.jpg' : `${sid}-hero.jpg`, `dl/${sid}-library_hero.jpg`, `dl/${sid}-wide.jpg`],
      // Downloads rows (mockup 07): the store's wide header capsule.
      header: [`dl/${sid}-header.jpg`, `${sid}-header.jpg`, `dl/${sid}-wide.jpg`],
      logo: [`${sid}-logo.png`],
      icon: [`icons/${sid}.jpg`],
    }
    const f = (c[kind] ?? []).map((x) => join(assets, x)).find((p) => existsSync(p) && p.startsWith(assets))
    return f ?? null
  }
  const storeArt = resolve(assets, 'store')
  return {
    name: 'dev-art',
    configureServer(server) {
      // The store mock's official art (mockups/assets/store/<id>-<kind>.jpg), same idea as /@dev-art.
      server.middlewares.use('/@dev-store', (req, res) => {
        const f = join(storeArt, decodeURIComponent((req.url ?? '').split('?')[0].replace(/^\//, '')))
        if (!f.startsWith(storeArt) || !existsSync(f)) { res.statusCode = 404; res.end(); return }
        res.setHeader('Content-Type', 'image/jpeg')
        createReadStream(f).pipe(res)
      })
      server.middlewares.use('/@dev-art', (req, res) => {
        const [, id, kind] = (req.url ?? '').split('?')[0].split('/')
        const f = id && kind ? pick(id, kind) : null
        if (!f) { res.statusCode = 404; res.end(); return }
        res.setHeader('Content-Type', f.endsWith('.png') ? 'image/png' : 'image/jpeg')
        createReadStream(f).pipe(res)
      })
    },
  }
}

export default defineConfig({
  plugins: [react(), devArt()],
  base: './',
  server: { port: 5180, strictPort: true, fs: { allow: [resolve(here, '../..')] } },
  clearScreen: false,
  build: { target: 'es2022', cssCodeSplit: true, chunkSizeWarningLimit: 400, reportCompressedSize: true },
})

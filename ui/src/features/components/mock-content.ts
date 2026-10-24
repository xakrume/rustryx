import type { components } from '@/api/schema'
import { repositories } from '@/mocks/store'

type RepositoryView = components['schemas']['RepositoryView']
type RepoFormat = components['schemas']['RepoFormat']

/**
 * Deterministic content of the mock repositories: components with assets, generated per
 * repository the first time it is asked for, from a generator seeded with the repository's
 * name. Browse, components, search, upload and delete all read this one store, so what the
 * tree shows is what the list shows and what a delete removes. It is a fixture, not
 * application code, so it keeps its state in plain module variables; `resetContent` clears it.
 */
export interface MockAsset {
  id: string
  componentId: string
  repository: string
  /** No leading slash, as `Asset.path` has it. */
  path: string
  contentType: string
  size: number
  sha1: string
  sha256: string
  md5: string
  createdAt: string
  lastDownloadedAt: string | null
  downloads: number
}

export interface MockComponent {
  id: string
  repository: string
  format: RepoFormat
  group: string | null
  name: string
  version: string | null
  createdAt: string
  attributes: Record<string, unknown>
  assets: MockAsset[]
}

const generated = new Map<string, MockComponent[]>()
const byId = new Map<string, MockComponent>()
const assetById = new Map<string, MockAsset>()

export function resetContent(): void {
  generated.clear()
  byId.clear()
  assetById.clear()
}

function hash(text: string): number {
  let h = 2166136261
  for (let i = 0; i < text.length; i += 1) h = Math.imul(h ^ text.charCodeAt(i), 16777619)
  return h >>> 0
}

function rng(seed: number): () => number {
  let s = seed || 1
  return () => {
    s = (Math.imul(s, 1664525) + 1013904223) >>> 0
    return s / 4294967296
  }
}

/** Hex digits of the requested length from a seed: the same input always gives the same digest. */
export function fakeDigest(seed: string, length: number): string {
  let out = ''
  let h = hash(seed)
  while (out.length < length) {
    h = Math.imul(h ^ (h >>> 15), 2246822507) >>> 0
    out += h.toString(16).padStart(8, '0')
  }
  return out.slice(0, length)
}

const hex = (n: number, width: number): string => n.toString(16).padStart(width, '0')

export function repoIndex(repo: RepositoryView): number {
  return Number.parseInt(repo.id.slice(-12), 16) - 0x100
}

const makeId = (kind: 'c' | 'a', repo: RepositoryView, n: number): string =>
  `0192f6c4-5a1e-7000-8000-${kind}${hex(repoIndex(repo), 5)}${hex(n, 6)}`

const minutes = (ago: number): string =>
  new Date(Date.now() - ago * 60_000).toISOString().replace(/\.\d{3}Z$/, 'Z')

interface Draft {
  group: string | null
  name: string
  version: string | null
  files: { path: string; type: string; size: number }[]
  attributes: Record<string, unknown>
}

const AREAS = ['core', 'auth', 'billing', 'search', 'ui', 'storage', 'events', 'metrics']
const NAMES = ['api', 'client', 'common', 'engine', 'gateway', 'models', 'router', 'worker']

function version(r: () => number, snapshot: boolean): string {
  const v = `${1 + Math.floor(r() * 3)}.${Math.floor(r() * 9)}.${Math.floor(r() * 12)}`
  return snapshot ? `${v}-SNAPSHOT` : v
}

function mavenDrafts(repo: RepositoryView, r: () => number): Draft[] {
  const snapshot = repo.name.includes('snapshot')
  const out: Draft[] = []
  for (let i = 0; i < (snapshot ? 18 : 64); i += 1) {
    const group = `com.example.${AREAS[i % AREAS.length]}`
    const name = `${NAMES[Math.floor(i / AREAS.length) % NAMES.length]}-${AREAS[(i * 3) % AREAS.length]}`
    const v = version(r, snapshot)
    const base = `${group.replaceAll('.', '/')}/${name}/${v}/${name}-${v}`
    out.push({
      group,
      name,
      version: v,
      attributes: { packaging: 'jar' },
      files: [
        {
          path: `${base}.jar`,
          type: 'application/java-archive',
          size: 40_000 + Math.floor(r() * 900_000),
        },
        { path: `${base}.pom`, type: 'application/xml', size: 1200 + Math.floor(r() * 2000) },
        {
          path: `${base}-sources.jar`,
          type: 'application/java-archive',
          size: 20_000 + Math.floor(r() * 300_000),
        },
      ],
    })
  }
  return out
}

function simpleDrafts(
  r: () => number,
  count: number,
  make: (i: number, v: string) => Draft,
): Draft[] {
  return Array.from({ length: count }, (_, i) => make(i, version(r, false)))
}

function draftsFor(repo: RepositoryView): Draft[] {
  const r = rng(hash(repo.name))
  const size = (): number => 8_000 + Math.floor(r() * 4_000_000)
  const team = /^team-(\d+)-/.exec(repo.name)
  // Every fifth team repository is empty, so the "nothing published yet" state has a home.
  if (team !== null && Number(team[1]) % 5 === 0) return []
  const small = team !== null ? 3 + (Number(team[1]) % 4) : repo.repo_type === 'proxy' ? 14 : 48
  switch (repo.format) {
    case 'maven2':
      return mavenDrafts(repo, r).slice(0, team !== null ? small : undefined)
    case 'npm':
      return simpleDrafts(r, small, (i, v) => ({
        group: '@acme',
        name: `${NAMES[i % NAMES.length]}-${AREAS[(i * 5) % AREAS.length]}`,
        version: v,
        attributes: { 'dist-tags': { latest: v } },
        files: [
          {
            path: `@acme/${NAMES[i % NAMES.length]}-${AREAS[(i * 5) % AREAS.length]}/-/${NAMES[i % NAMES.length]}-${AREAS[(i * 5) % AREAS.length]}-${v}.tgz`,
            type: 'application/octet-stream',
            size: size(),
          },
        ],
      }))
    case 'pypi':
      return simpleDrafts(r, small, (i, v) => {
        const name = `acme-${NAMES[i % NAMES.length]}`
        return {
          group: null,
          name,
          version: v,
          attributes: { requires_python: '>=3.10', summary: `The ${name} library` },
          files: [
            {
              path: `packages/${name}/${v}/${name}-${v}-py3-none-any.whl`,
              type: 'application/zip',
              size: size(),
            },
            {
              path: `packages/${name}/${v}/${name}-${v}.tar.gz`,
              type: 'application/gzip',
              size: size(),
            },
          ],
        }
      })
    case 'docker':
    case 'oci':
      return simpleDrafts(r, small, (i) => {
        const image = `acme/${NAMES[i % NAMES.length]}`
        const tag = i % 3 === 0 ? 'latest' : `1.${i}.0`
        const digest = `sha256:${fakeDigest(`${repo.name}${image}${tag}`, 64)}`
        return {
          group: 'acme',
          name: image,
          version: tag,
          attributes: {
            digest,
            tags: i % 3 === 0 ? ['latest', `1.${i}.0`] : [tag],
            media_type: 'application/vnd.oci.image.manifest.v1+json',
            architecture: 'amd64',
            os: 'linux',
          },
          files: [
            {
              path: `v2/${image}/manifests/${tag}`,
              type: 'application/vnd.oci.image.manifest.v1+json',
              size: 1800,
            },
            {
              path: `v2/${image}/blobs/${digest}`,
              type: 'application/vnd.oci.image.layer.v1.tar+gzip',
              size: size() * 6,
            },
          ],
        }
      })
    case 'helm':
      return simpleDrafts(r, small, (i, v) => {
        const name = `${NAMES[i % NAMES.length]}-chart`
        return {
          group: null,
          name,
          version: v,
          attributes: { appVersion: v, apiVersion: 'v2' },
          files: [
            {
              path: `charts/${name}-${v}.tgz`,
              type: 'application/gzip',
              size: 12_000 + Math.floor(r() * 90_000),
            },
          ],
        }
      })
    case 'pub':
      return simpleDrafts(r, small, (i, v) => {
        const name = `acme_${NAMES[i % NAMES.length]}`
        return {
          group: null,
          name,
          version: v,
          attributes: { sdk: '>=3.0.0 <4.0.0' },
          files: [
            {
              path: `packages/${name}/versions/${v}.tar.gz`,
              type: 'application/gzip',
              size: size(),
            },
          ],
        }
      })
    default: {
      const files = [
        ['docs/README.txt', 'text/plain'],
        ['docs/handbook.pdf', 'application/pdf'],
        ['images/logo.png', 'image/png'],
        ['images/banner.svg', 'image/svg+xml'],
        ['releases/app-1.0.0.zip', 'application/zip'],
        ['releases/app-1.1.0.zip', 'application/zip'],
        ['releases/missing-blob.bin', 'application/octet-stream'],
        ['scripts/install.sh', 'text/x-shellscript'],
        ['data/export.csv', 'text/csv'],
      ] as const
      return files.slice(0, team !== null ? 4 : files.length).map(([path, type]) => ({
        group: path.split('/')[0] ?? null,
        name: path.split('/').pop() ?? path,
        version: null,
        attributes: {},
        files: [{ path, type, size: size() }],
      }))
    }
  }
}

function materialise(repo: RepositoryView, drafts: Draft[]): MockComponent[] {
  return drafts.map((draft, index) => {
    const id = makeId('c', repo, index + 1)
    const created = minutes(10 + index * 37)
    const component: MockComponent = {
      id,
      repository: repo.name,
      format: repo.format,
      group: draft.group,
      name: draft.name,
      version: draft.version,
      createdAt: created,
      attributes: draft.attributes,
      assets: draft.files.map((file, fileIndex) => ({
        id: makeId('a', repo, (index + 1) * 16 + fileIndex),
        componentId: id,
        repository: repo.name,
        path: file.path,
        contentType: file.type,
        size: file.size,
        sha1: fakeDigest(`${file.path}sha1`, 40),
        sha256: fakeDigest(`${file.path}sha256`, 64),
        md5: fakeDigest(`${file.path}md5`, 32),
        createdAt: created,
        lastDownloadedAt: index % 4 === 0 ? null : minutes(index * 53),
        downloads: index % 4 === 0 ? 0 : 3 + ((index * 7) % 90),
      })),
    }
    return component
  })
}

function register(components: readonly MockComponent[]): void {
  for (const c of components) {
    byId.set(c.id, c)
    for (const a of c.assets) assetById.set(a.id, a)
  }
}

/** Components that live in the repository itself (a group has none of its own). */
export function ownComponents(repo: RepositoryView): MockComponent[] {
  if (repo.repo_type === 'group') return []
  let list = generated.get(repo.name)
  if (list === undefined) {
    list = materialise(repo, draftsFor(repo))
    generated.set(repo.name, list)
    register(list)
  }
  return list
}

/** What a client sees at the repository: its own content, or for a group the members merged. */
export function visibleComponents(repo: RepositoryView, seen = new Set<string>()): MockComponent[] {
  if (repo.repo_type !== 'group') return [...ownComponents(repo)]
  if (seen.has(repo.name)) return []
  seen.add(repo.name)
  const paths = new Set<string>()
  const merged: MockComponent[] = []
  for (const memberName of repo.group?.members ?? []) {
    const member = repositories.find((r) => r.name === memberName)
    if (member === undefined) continue
    for (const component of visibleComponents(member, seen)) {
      // First member wins on a path, as the real group does.
      if (component.assets.some((a) => paths.has(a.path))) continue
      component.assets.forEach((a) => paths.add(a.path))
      merged.push(component)
    }
  }
  return merged
}

export function findComponent(id: string): MockComponent | undefined {
  const known = byId.get(id)
  if (known !== undefined) return known
  // A deep link before the repository was ever listed: derive the repository from the id.
  const match = /^0192f6c4-5a1e-7000-8000-[ca]([0-9a-f]{5})[0-9a-f]{6}$/.exec(id)
  const index = match?.[1] === undefined ? -1 : Number.parseInt(match[1], 16)
  const repo = repositories.find((r) => repoIndex(r) === index)
  if (repo !== undefined) ownComponents(repo)
  return byId.get(id)
}

export function findAsset(id: string): MockAsset | undefined {
  const known = assetById.get(id)
  if (known !== undefined) return known
  const match = /^0192f6c4-5a1e-7000-8000-a([0-9a-f]{5})[0-9a-f]{6}$/.exec(id)
  const index = match?.[1] === undefined ? -1 : Number.parseInt(match[1], 16)
  const repo = repositories.find((r) => repoIndex(r) === index)
  if (repo !== undefined) ownComponents(repo)
  return assetById.get(id)
}

export function removeComponent(component: MockComponent): void {
  const list = generated.get(component.repository)
  if (list !== undefined) {
    generated.set(
      component.repository,
      list.filter((c) => c.id !== component.id),
    )
  }
  byId.delete(component.id)
  component.assets.forEach((a) => assetById.delete(a.id))
}

export function removeAsset(asset: MockAsset): void {
  const component = byId.get(asset.componentId)
  if (component === undefined) return
  const rest = component.assets.filter((a) => a.id !== asset.id)
  assetById.delete(asset.id)
  if (rest.length === 0) {
    removeComponent(component)
    return
  }
  const next = { ...component, assets: rest }
  byId.set(component.id, next)
  const list = generated.get(component.repository) ?? []
  generated.set(
    component.repository,
    list.map((c) => (c.id === component.id ? next : c)),
  )
}

let uploads = 0

/** Paths among `paths` that already exist in the repository: what an upload would replace. */
export function existingPaths(repo: RepositoryView, paths: readonly string[]): string[] {
  const wanted = new Set(paths)
  return ownComponents(repo).flatMap((c) =>
    c.assets.map((a) => a.path).filter((p) => wanted.has(p)),
  )
}

/** Add a published component (upload). Returns it with its assets registered. */
export function addComponent(
  repo: RepositoryView,
  draft: Omit<Draft, 'files'> & {
    files: { path: string; type: string; size: number; sha256?: string }[]
  },
): { component: MockComponent; replaced: string[] } {
  ownComponents(repo)
  uploads += 1
  const id = makeId('c', repo, 0x800 + uploads)
  // Newer than every seeded component, so a fresh upload leads the newest first lists.
  const now = minutes(-uploads)
  const replaced = existingPaths(
    repo,
    draft.files.map((f) => f.path),
  )
  // Write policy `allow` replaces the old file: it leaves the repository as the new one arrives.
  for (const existing of [...ownComponents(repo)]) {
    for (const a of [...existing.assets]) if (replaced.includes(a.path)) removeAsset(a)
  }
  const component: MockComponent = {
    id,
    repository: repo.name,
    format: repo.format,
    group: draft.group,
    name: draft.name,
    version: draft.version,
    createdAt: now,
    attributes: draft.attributes,
    assets: draft.files.map((file, i) => ({
      id: makeId('a', repo, 0x8000 + uploads * 16 + i),
      componentId: id,
      repository: repo.name,
      path: file.path,
      contentType: file.type,
      size: file.size,
      sha1: fakeDigest(`${file.path}sha1`, 40),
      sha256: file.sha256 ?? fakeDigest(`${file.path}${file.size}sha256`, 64),
      md5: fakeDigest(`${file.path}md5`, 32),
      createdAt: now,
      lastDownloadedAt: null,
      downloads: 0,
    })),
  }
  generated.set(repo.name, [component, ...(generated.get(repo.name) ?? [])])
  register([component])
  return { component, replaced }
}

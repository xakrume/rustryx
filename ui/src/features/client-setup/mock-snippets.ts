import type { components } from '@/api/schema'

type ClientSnippet = components['schemas']['ClientSnippet']
type RepoFormat = components['schemas']['RepoFormat']

/**
 * What the real server builds from templates next to the format crates, for the in-browser
 * mock: structured snippets with `<NAME>` placeholders and never a real credential. The same
 * shape and rules as docs/ui-page-inventory.md section 9: an anonymous variant needs no
 * credentials, publish snippets exist only for callers who may write, and a group resolves
 * through itself and publishes to its hosted member.
 */
export interface SnippetInput {
  format: RepoFormat
  name: string
  /** Base URL of the repository, with a trailing slash. */
  url: string
  /** Where clients publish: the repository itself, or the hosted member of a group. */
  publishUrl: string | null
  publishName: string | null
  anonymous: boolean
  canWrite: boolean
}

const USERNAME = { name: 'USERNAME', description: 'Your Rustryx user name.' }
const TOKEN = {
  name: 'TOKEN',
  description: 'A personal token with the access this step needs. Never commit it.',
}

type Draft = Omit<ClientSnippet, 'placeholders'> & { placeholders?: ClientSnippet['placeholders'] }

const snippet = (draft: Draft): ClientSnippet => ({ placeholders: [], ...draft })

function host(url: string): string {
  return new URL(url).host
}

function maven(i: SnippetInput): ClientSnippet[] {
  const out: ClientSnippet[] = [
    snippet({
      id: 'mvn-settings',
      client: 'mvn',
      title: 'Resolve through the repository',
      purpose: 'resolve',
      language: 'xml',
      file: '~/.m2/settings.xml',
      content: `<mirror>\n  <id>rustryx</id>\n  <mirrorOf>*</mirrorOf>\n  <url>${i.url}</url>\n</mirror>`,
      requires: 'browse',
    }),
  ]
  if (!i.anonymous) {
    out.push(
      snippet({
        id: 'mvn-credentials',
        client: 'mvn',
        title: 'Credentials',
        purpose: 'login',
        language: 'xml',
        file: '~/.m2/settings.xml',
        content:
          '<server>\n  <id>rustryx</id>\n  <username><USERNAME></username>\n  <password><TOKEN></password>\n</server>',
        placeholders: [USERNAME, TOKEN],
        requires: 'browse',
      }),
    )
  }
  out.push(
    snippet({
      id: 'gradle-resolve',
      client: 'gradle',
      title: 'Resolve with Gradle',
      purpose: 'resolve',
      language: 'groovy',
      file: 'build.gradle',
      content: i.anonymous
        ? `repositories {\n    maven { url = uri("${i.url}") }\n}`
        : `repositories {\n    maven {\n        url = uri("${i.url}")\n        credentials {\n            username = "<USERNAME>"\n            password = "<TOKEN>"\n        }\n    }\n}`,
      placeholders: i.anonymous ? [] : [USERNAME, TOKEN],
      requires: 'browse',
    }),
  )
  if (i.canWrite && i.publishUrl) {
    out.push(
      snippet({
        id: 'mvn-publish',
        client: 'mvn',
        title: 'Publish with mvn deploy',
        purpose: 'publish',
        language: 'xml',
        file: 'pom.xml',
        content: `<distributionManagement>\n  <repository>\n    <id>rustryx</id>\n    <url>${i.publishUrl}</url>\n  </repository>\n</distributionManagement>`,
        requires: 'write',
      }),
    )
  }
  return out
}

function npm(i: SnippetInput): ClientSnippet[] {
  const registry = `registry=${i.url}`
  const auth = `//${host(i.url)}/repository/${i.name}/:_authToken=<TOKEN>`
  const out = [
    snippet({
      id: 'npm-registry',
      client: 'npm',
      title: 'Resolve through the repository',
      purpose: 'resolve',
      language: 'ini',
      file: '~/.npmrc',
      content: i.anonymous ? registry : `${registry}\n${auth}`,
      placeholders: i.anonymous ? [] : [TOKEN],
      requires: 'browse',
    }),
  ]
  if (i.canWrite && i.publishUrl) {
    out.push(
      snippet({
        id: 'npm-publish',
        client: 'npm',
        title: 'Publish a package',
        purpose: 'publish',
        language: 'bash',
        file: null,
        content: `npm publish --registry ${i.publishUrl}`,
        requires: 'write',
      }),
    )
  }
  return out
}

function pypi(i: SnippetInput): ClientSnippet[] {
  const simple = `${i.url}simple/`
  const credential = i.anonymous ? '' : '<USERNAME>:<TOKEN>@'
  const indexUrl = simple.replace('://', `://${credential}`)
  const out = [
    snippet({
      id: 'pip-index',
      client: 'pip',
      title: 'Resolve with pip',
      purpose: 'resolve',
      language: 'ini',
      file: '~/.config/pip/pip.conf',
      content: `[global]\nindex-url = ${indexUrl}`,
      placeholders: i.anonymous ? [] : [USERNAME, TOKEN],
      requires: 'browse',
    }),
  ]
  if (i.canWrite && i.publishUrl) {
    out.push(
      snippet({
        id: 'twine-upload',
        client: 'twine',
        title: 'Publish with twine',
        purpose: 'publish',
        language: 'bash',
        file: null,
        content: `twine upload --repository-url ${i.publishUrl} -u <USERNAME> -p <TOKEN> dist/*`,
        placeholders: [USERNAME, TOKEN],
        requires: 'write',
      }),
    )
  }
  return out
}

function docker(i: SnippetInput): ClientSnippet[] {
  const registry = `${host(i.url)}/${i.name}`
  const out: ClientSnippet[] = []
  if (!i.anonymous) {
    out.push(
      snippet({
        id: 'docker-login',
        client: 'docker',
        title: 'Sign in',
        purpose: 'login',
        language: 'bash',
        file: null,
        content: `docker login ${host(i.url)} -u <USERNAME> -p <TOKEN>`,
        placeholders: [USERNAME, TOKEN],
        requires: 'browse',
      }),
    )
  }
  out.push(
    snippet({
      id: 'docker-pull',
      client: 'docker',
      title: 'Pull an image',
      purpose: 'resolve',
      language: 'bash',
      file: null,
      content: `docker pull ${registry}/<IMAGE>:<TAG>`,
      placeholders: [
        { name: 'IMAGE', description: 'Image name inside the repository.' },
        { name: 'TAG', description: 'Tag or digest to pull.' },
      ],
      requires: 'browse',
    }),
  )
  if (i.canWrite && i.publishName) {
    const target = `${host(i.url)}/${i.publishName}`
    out.push(
      snippet({
        id: 'docker-push',
        client: 'docker',
        title: 'Tag and push an image',
        purpose: 'publish',
        language: 'bash',
        file: null,
        content: `docker tag <IMAGE>:<TAG> ${target}/<IMAGE>:<TAG>\ndocker push ${target}/<IMAGE>:<TAG>`,
        placeholders: [
          { name: 'IMAGE', description: 'Image name inside the repository.' },
          { name: 'TAG', description: 'Tag or digest to pull.' },
        ],
        requires: 'write',
      }),
    )
  }
  return out
}

function helm(i: SnippetInput): ClientSnippet[] {
  const out: ClientSnippet[] = []
  if (!i.anonymous) {
    out.push(
      snippet({
        id: 'helm-login',
        client: 'helm',
        title: 'Sign in to the registry',
        purpose: 'login',
        language: 'bash',
        file: null,
        content: `helm registry login ${host(i.url)} --username <USERNAME> --password <TOKEN>`,
        placeholders: [USERNAME, TOKEN],
        requires: 'browse',
      }),
    )
  }
  out.push(
    snippet({
      id: 'helm-pull',
      client: 'helm',
      title: 'Pull a chart',
      purpose: 'resolve',
      language: 'bash',
      file: null,
      content: `helm pull oci://${host(i.url)}/${i.name}/<CHART> --version <VERSION>\nhelm repo add rustryx ${i.url}`,
      placeholders: [
        { name: 'CHART', description: 'Chart name.' },
        { name: 'VERSION', description: 'Chart version.' },
      ],
      requires: 'browse',
    }),
  )
  if (i.canWrite && i.publishName) {
    out.push(
      snippet({
        id: 'helm-push',
        client: 'helm',
        title: 'Push a chart',
        purpose: 'publish',
        language: 'bash',
        file: null,
        content: `helm push <CHART>-<VERSION>.tgz oci://${host(i.url)}/${i.publishName}`,
        placeholders: [
          { name: 'CHART', description: 'Chart name.' },
          { name: 'VERSION', description: 'Chart version.' },
        ],
        requires: 'write',
      }),
    )
  }
  return out
}

function pub(i: SnippetInput): ClientSnippet[] {
  const out = [
    snippet({
      id: 'dart-hosted',
      client: 'dart',
      title: 'Resolve from the repository',
      purpose: 'resolve',
      language: 'yaml',
      file: 'pubspec.yaml',
      content: `dependencies:\n  <PACKAGE>:\n    hosted: ${i.url}\n    version: ^<VERSION>`,
      placeholders: [
        { name: 'PACKAGE', description: 'Package name.' },
        { name: 'VERSION', description: 'Package version.' },
      ],
      requires: 'browse',
    }),
  ]
  if (!i.anonymous) {
    out.unshift(
      snippet({
        id: 'dart-token',
        client: 'dart',
        title: 'Store the token',
        purpose: 'login',
        language: 'bash',
        file: null,
        content: `echo "<TOKEN>" | dart pub token add ${i.url}`,
        placeholders: [TOKEN],
        requires: 'browse',
      }),
    )
  }
  if (i.canWrite && i.publishUrl) {
    out.push(
      snippet({
        id: 'dart-publish',
        client: 'dart',
        title: 'Publish a package',
        purpose: 'publish',
        language: 'bash',
        file: null,
        content: `dart pub publish --server ${i.publishUrl}`,
        requires: 'write',
      }),
    )
  }
  return out
}

function raw(i: SnippetInput): ClientSnippet[] {
  const out = [
    snippet({
      id: 'curl-download',
      client: 'curl',
      title: 'Download a file',
      purpose: 'resolve',
      language: 'bash',
      file: null,
      content: i.anonymous
        ? `curl -fO ${i.url}<PATH>`
        : `curl -fO -u <USERNAME>:<TOKEN> ${i.url}<PATH>`,
      placeholders: [
        ...(i.anonymous ? [] : [USERNAME, TOKEN]),
        { name: 'PATH', description: 'Path of the file inside the repository.' },
      ],
      requires: 'browse',
    }),
  ]
  if (i.canWrite && i.publishUrl) {
    out.push(
      snippet({
        id: 'curl-upload',
        client: 'curl',
        title: 'Upload a file',
        purpose: 'publish',
        language: 'bash',
        file: null,
        content: `curl -fT <FILE> -u <USERNAME>:<TOKEN> ${i.publishUrl}<PATH>`,
        placeholders: [
          USERNAME,
          TOKEN,
          { name: 'FILE', description: 'Local file to upload.' },
          { name: 'PATH', description: 'Path of the file inside the repository.' },
        ],
        requires: 'write',
      }),
    )
  }
  return out
}

export function buildSnippets(input: SnippetInput): ClientSnippet[] {
  switch (input.format) {
    case 'maven2':
      return maven(input)
    case 'npm':
      return npm(input)
    case 'pypi':
      return pypi(input)
    case 'docker':
    case 'oci':
      return docker(input)
    case 'helm':
      return helm(input)
    case 'pub':
      return pub(input)
    case 'raw':
      return raw(input)
    case 'rpm':
      return [
        snippet({
          id: 'dnf-repo',
          client: 'dnf',
          title: 'Use the repository with dnf or yum',
          purpose: 'resolve',
          language: 'ini',
          file: '/etc/yum.repos.d/rustryx.repo',
          content: `[rustryx]\nname=rustryx\nbaseurl=${input.url}\nenabled=1\ngpgcheck=0\n`,
          requires: 'browse',
        }),
      ]
    case 'apt':
      return [
        snippet({
          id: 'apt-source',
          client: 'apt',
          title: 'Add the repository to apt',
          purpose: 'resolve',
          language: 'text',
          file: '/etc/apt/sources.list.d/rustryx.list',
          content: `deb ${input.url} <SUITE> main`,
          placeholders: [
            { name: 'SUITE', description: 'Distribution suite, for example trixie or noble.' },
          ],
          requires: 'browse',
        }),
      ]
    case 'apk':
      return [
        snippet({
          id: 'apk-repo',
          client: 'apk',
          title: 'Add the repository to apk',
          purpose: 'resolve',
          language: 'text',
          file: '/etc/apk/repositories',
          content: input.url,
          requires: 'browse',
        }),
      ]
    default:
      return []
  }
}

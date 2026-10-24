import { delay, http, type HttpHandler } from 'msw'
import type { components } from '@/api/schema'
import { findVisible, permissionsFor, sessionUser } from '@/features/repositories/mock-state'
import { problem } from '@/mocks/problem'
import { json, LATENCY_MS, repositories } from '@/mocks/store'
import { buildSnippets } from './mock-snippets'

type ClientSetup = components['schemas']['ClientSetup']

/**
 * `GET /api/v1/client-setup?repository=`. The base URL is the part of the repository URL before
 * `/repository/`. A group publishes to its first hosted member, and a repository named
 * `*-private-ca` reports a private CA so the note can be seen without a backend switch.
 */
export const handlers: HttpHandler[] = [
  http.get('/api/v1/client-setup', async ({ request }) => {
    await delay(LATENCY_MS)
    const name = new URL(request.url).searchParams.get('repository')
    if (name === null || name === '') {
      return problem(400, 'invalid_query', 'The repository parameter is required')
    }
    const repo = findVisible(name)
    if (repo === undefined) return problem(404, 'repository_not_found', 'Repository not found')

    const user = sessionUser()
    const anonymous = repo.anonymous_effective
    const baseUrl = repo.url.split('/repository/')[0] ?? repo.url
    const hostedMember =
      repo.repo_type === 'group'
        ? repo.group?.members
            .map((member) => repositories.find((r) => r.name === member))
            .find((r) => r?.repo_type === 'hosted')
        : undefined
    const publishRepo = repo.repo_type === 'hosted' ? repo : hostedMember
    const body: ClientSetup = {
      repository: { id: repo.id, name: repo.name, format: repo.format, repo_type: repo.repo_type },
      base_url: baseUrl,
      base_url_configured: !repo.name.includes('no-base-url'),
      variant: anonymous ? 'anonymous' : 'authenticated',
      publish_target:
        repo.repo_type === 'group' && hostedMember !== undefined
          ? {
              repository: hostedMember.name,
              reason: 'Group repositories are read only; publish to the hosted member.',
            }
          : null,
      snippets: buildSnippets({
        format: repo.format,
        name: repo.name,
        url: repo.url,
        publishUrl: publishRepo?.url ?? null,
        publishName: publishRepo?.name ?? null,
        anonymous,
        canWrite: publishRepo !== undefined && permissionsFor(publishRepo, user).write,
      }),
      notes: repo.online
        ? []
        : [
            {
              kind: 'warning',
              text: 'This repository is offline. Reads answer 503 until it is switched online.',
            },
          ],
      private_ca: repo.name.includes('private-ca'),
    }
    return json(body)
  }),
]

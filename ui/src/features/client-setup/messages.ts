/**
 * Strings of the client setup panel. Merged into the catalog by `contentMessages`
 * (features/repositories/content-messages.ts), which i18n/en.ts spreads in.
 */
export const clientSetupMessages = {
  clientSetup: {
    title: 'Client setup',
    loadFailed: 'The client setup could not be loaded',
    clients: 'Client',
    families: {
      maven: 'Maven',
      npm: 'npm',
      pip: 'pip',
      docker: 'Docker',
      helm: 'Helm',
      dart: 'Dart pub',
      raw: 'curl',
    },
    purpose: {
      resolve: 'Resolve',
      publish: 'Publish',
      login: 'Sign in',
    },
    file: 'File',
    showRaw: 'Show raw text',
    placeholders: 'Placeholders',
    placeholderHelp: 'Replace each marked value before you run a snippet.',
    variantAnonymous:
      'Anonymous read is on for this repository, so resolving needs no credentials.',
    variantAuthenticated:
      'Resolving needs credentials. Use your user name and a personal token as the password.',
    publishTarget:
      'This is a group repository. Resolve through the group and publish to the hosted member.',
    baseUrlMissing:
      'The server base URL is not configured, so the snippets use this browser address ({url}).',
    setBaseUrl: 'Set the base URL',
    privateCa:
      'This server uses a private certificate authority. Install its certificate on every client before you use these snippets.',
    noSnippets: 'This format has no client template yet. The repository URL is {url}',
    createToken: 'Create a token for me',
    removeToken: 'Remove the token from the snippets',
    tokenInUse:
      'The snippets use your new token for this page view only. It is not stored; reload the page to remove it.',
    tokenWhat: 'API token',
    tokenNameDefault: 'client-setup-{repository}',
    tokenDialog: {
      title: 'Create a token for these snippets',
      description: 'A personal token limited to the repository {name}. It is shown once.',
      name: 'Token name',
      nameRequired: 'Enter a name for the token.',
      expires: 'Expires in (days)',
      expiresHelper: 'Between 1 and {max} days.',
      daysInvalid: 'Enter a whole number of days between 1 and {max}.',
      access: 'Access',
      access_read: 'Read only (browse and download)',
      access_write: 'Read and publish',
      create: 'Create token',
      failed: 'The token could not be created',
    },
  },
} as const

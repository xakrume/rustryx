export const anonymousMessages = {
  anonymous: {
    description:
      'Anonymous callers can read a repository only when the global switch AND the flag of that repository are both on. They never get write access.',
    global: {
      title: 'Global switch',
      label: 'Allow anonymous access',
      on: 'On',
      off: 'Off',
      readOnlyTitle: 'This switch is read only in this version',
      readOnlyBody:
        'It comes from the configuration (auth.anonymous_enabled), not from the database. To change it, change the configuration and restart or reload the nodes. The repository flags below can be changed here.',
      role: 'Anonymous callers act as the role',
      publicCount: {
        one: '{count} repository has its flag on',
        other: '{count} repositories have their flag on',
      },
      effectiveCount: {
        one: '{count} repository is readable without signing in right now',
        other: '{count} repositories are readable without signing in right now',
      },
      offExplain:
        'The global switch is off, so every repository is private to anonymous callers, whatever its flag says.',
      enableTitle: 'Allow anonymous access?',
      enableBody:
        'Anyone who can reach this server will be able to browse and download the repositories whose flag is on, without an account.',
      enableConfirm: 'Allow anonymous access',
      disableTitle: 'Turn anonymous access off?',
      disableBody: 'Every repository becomes private to anonymous callers at once.',
      disableConfirm: 'Turn it off',
      saved: 'Anonymous access is now {state}',
      failed: 'The global switch could not be changed',
    },
    repositories: {
      title: 'Repositories',
      tableLabel: 'Anonymous access per repository',
      filters: {
        search: 'Repository name contains',
        format: 'Format',
        flag: 'Flag',
        flagOn: 'Flag on',
        flagOff: 'Flag off',
      },
      columns: {
        name: 'Repository',
        format: 'Format',
        type: 'Type',
        flag: 'Allow anonymous',
        effective: 'Effective result',
      },
      flagLabel: 'Allow anonymous access to {name}',
      effective: {
        public: 'Public: anyone can read',
        globalOff: 'Private: the flag is on but the global switch is off',
        private: 'Private',
      },
      empty: {
        title: 'No repositories',
        body: 'Create a repository first. Its anonymous flag is off by default.',
      },
      bulkAllow: 'Allow anonymous',
      bulkDisallow: 'Disallow anonymous',
      selectionLabel: 'Select {name}',
    },
    expose: {
      title: 'Make repositories public?',
      body: {
        one: 'Anyone will be able to browse and download {names} without an account, as soon as the global switch is on.',
        other:
          'Anyone will be able to browse and download these {count} repositories without an account, as soon as the global switch is on: {names}.',
      },
      confirm: 'Make public',
      more: '{names} and {more} more',
    },
    changed: {
      allowed: {
        one: 'Anonymous access allowed for {count} repository',
        other: 'Anonymous access allowed for {count} repositories',
      },
      disallowed: {
        one: 'Anonymous access removed from {count} repository',
        other: 'Anonymous access removed from {count} repositories',
      },
      failed: 'The repository flags could not be changed',
    },
  },
} as const

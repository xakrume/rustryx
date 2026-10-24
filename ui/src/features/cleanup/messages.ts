/** Strings of the cleanup policy pages. */
export const cleanupMessages = {
  title: 'Cleanup policies',
  description:
    'Rules that remove old components from repositories. A policy applies to repositories of its format that have it attached.',
  tableLabel: 'Cleanup policies',
  create: 'Create policy',
  createTitle: 'Create a cleanup policy',
  editTitle: 'Edit cleanup policy {name}',
  backToList: 'Back to policies',
  filters: {
    name: 'Name starts with',
    namePlaceholder: 'maven, npm',
    format: 'Format',
  },
  columns: {
    name: 'Name',
    format: 'Format',
    criteria: 'Criteria',
    repositories: 'Attached to',
    lastRun: 'Last run',
    updated: 'Updated',
    actions: 'Actions',
  },
  empty: {
    title: 'No cleanup policies yet',
    description:
      'A policy removes components that are old, unused or surplus. Start with a dry run to see what it would remove before any repository uses it.',
  },
  noRepositories: 'No repository',
  lastRun: {
    none: 'Never run',
    dry: 'Dry run',
    real: 'Run',
    candidates: '{count} items, {size}',
  },
  actions: {
    edit: 'Edit {name}',
    delete: 'Delete {name}',
    openRuns: 'View all runs',
  },
  format: {
    maven2: 'Maven',
    npm: 'npm',
    pypi: 'PyPI',
    docker: 'Docker',
    oci: 'OCI',
    helm: 'Helm',
    raw: 'Raw',
    pub: 'Dart pub',
  },
  criteria: {
    legend: 'Criteria',
    help: 'A component is removed only when ALL set criteria match. Leave a criterion empty to ignore it.',
    atLeastOne:
      'Set at least one criterion. A policy without criteria would match every component.',
    unsupported: 'Not supported for {format} repositories.',
    formatsUnavailable:
      'The criteria each format supports could not be loaded. The server checks them when you save.',
    unsupportedCleared: 'This criterion is not supported for {format} and was cleared.',
    older_than_days: {
      label: 'Older than (days)',
      helper: 'Component age since it was created.',
      summary: 'older than {value} days',
    },
    last_downloaded_days: {
      label: 'Not downloaded for (days)',
      helper:
        'Days since the last download. Components that were never downloaded count from their publish date.',
      summary: 'not downloaded for {value} days',
    },
    keep_last_versions: {
      label: 'Keep the newest versions of a package',
      helper: 'Versions of one package to keep, newest first. Older versions match.',
      summary: 'older than the newest {value} versions of its package',
    },
    name_pattern: {
      label: 'Component name pattern',
      helper:
        'Regular expression matched against the whole component name (linear time engine, no backreferences or lookaround).',
      summary: 'name matches {value}',
    },
    release_type: {
      label: 'Release type',
      helper: 'Maven only: which kind of version to match.',
      any: 'Any',
      release: 'release versions',
      prerelease: 'pre-release versions',
      optionRelease: 'Release versions',
      optionPrerelease: 'Pre-release versions (snapshots)',
      summary: '{value} only',
    },
    summaryLead: 'A component is removed when all of these are true:',
    summaryNone: 'No criterion set yet.',
  },
  fields: {
    name: 'Name',
    nameHelper: 'Shown in repository settings and in reports.',
    description: 'Description',
    format: 'Format',
    formatHelper: 'The kind of repositories the policy can be attached to.',
    formatLocked: {
      one: 'Locked: {count} repository uses this policy, so its format cannot change.',
      other: 'Locked: {count} repositories use this policy, so its format cannot change.',
    },
  },
  attached: {
    title: 'Attached repositories',
    none: 'No repository uses this policy yet. Attach it in the settings of a repository.',
    note: 'Attach or detach a policy in the settings of each repository.',
  },
  preview: {
    title: 'Preview',
    description:
      'Runs the saved policy as a dry run and lists what it would remove, with counts and sizes. Nothing is deleted.',
    start: 'Preview what would be removed',
    starting: 'Starting',
    saveFirst: 'Save the policy first: the preview runs a saved policy.',
    saveChanges: 'Save your changes first: the preview runs the saved version, not the form.',
    alreadyRunning: 'A dry run of this policy is already running.',
    showRunning: 'Show the running dry run',
    started: 'Dry run started',
  },
  delete: {
    description:
      'The policy is removed. Components that already matched stay where they are; nothing is deleted from repositories.',
    inUse: {
      one: '{count} repository uses it: {names}. Deleting detaches the policy from it, so it stops cleaning up.',
      other:
        '{count} repositories use it: {names}. Deleting detaches the policy from them, so they stop cleaning up.',
    },
    confirmDetach: 'Detach and delete',
    done: 'Policy {name} deleted',
  },
  toast: {
    created: 'Policy {name} created',
    saved: 'Policy {name} saved',
  },
  save: 'Save policy',
  saving: 'Saving',
  discard: 'Discard changes',
  errors: {
    nameRequired: 'Enter a name.',
    nameLength: 'Use at most 128 characters.',
    number: 'Enter a whole number of 1 or more.',
    pattern: 'Enter a pattern.',
    patternLength: 'Use at most 512 characters.',
  },
} as const

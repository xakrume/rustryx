/** Words several identity pages use: where an account and a role come from, and common actions. */
export const sharedMessages = {
  identity: {
    origin: {
      manual: 'Manual',
      oidc: 'OIDC group',
      ldap: 'LDAP group',
      manualHint: 'Assigned by an administrator',
      oidcHint: 'Granted by an OIDC group of the last sign in',
      ldapHint: 'Granted by an LDAP group of the last sign in',
    },
    source: {
      local: 'Local',
      oidc: 'OIDC',
      ldap: 'LDAP',
    },
    format: {
      any: 'Any format',
    },
    action: {
      browse: 'Browse',
      read: 'Read',
      write: 'Write',
      delete: 'Delete',
    },
    grantKind: {
      admin: 'Admin',
      'repository-view': 'Repository view',
      'repository-content-selector': 'Content selector',
    },
    builtin: 'Built-in',
    builtinReadOnly: 'Built-in, read only',
    allRepositories: 'All repositories',
    wildcard: 'Wildcard',
    never: 'Never',
    noValue: 'None',
    picker: {
      search: 'Filter {what}',
      selected: {
        one: '{count} selected',
        other: '{count} selected',
      },
      none: 'Nothing matches the filter.',
      empty: 'There is nothing to choose from.',
    },
    adminOnly: 'Only administrators can change this.',
    loadFailed: 'The data could not be loaded',
  },
} as const

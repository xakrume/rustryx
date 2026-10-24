export const mappingsMessages = {
  mappings: {
    description:
      'Map groups of the identity provider to roles. A user receives the roles of every group they belong to when they sign in.',
    create: 'Add mapping',
    tabs: { label: 'Identity provider', oidc: 'OIDC groups', ldap: 'LDAP groups' },
    adminGroups: {
      title: 'Administrators come only from the configuration',
      body: 'nx-admin is never granted by a mapping. It comes only from the admin groups in the configuration file, so a mistake in this table cannot create an administrator.',
      groups: 'Admin groups of the configuration:',
      none: 'No admin groups are configured for this provider.',
      unknown: 'The admin groups could not be read.',
      noteDefaultRole: 'A user who matches nothing is refused when there is no default role.',
    },
    exact: {
      title: 'Matching is exact and case sensitive',
      body: 'Enter the full Keycloak group path (/org/team/group) or the full LDAP distinguished name. Nothing is trimmed, folded to lower case or matched with wildcards, so a typo or a different case never matches.',
    },
    tableLabel: 'Group mappings',
    columns: {
      order: 'Order',
      group: 'Group',
      role: 'Role',
      actions: 'Actions',
    },
    priority: 'Priority {priority}',
    moveUp: 'Move {group} up',
    moveDown: 'Move {group} down',
    moved: 'Moved {group} to position {position} of {count}',
    moveFailed: 'The order could not be changed',
    edit: 'Edit mapping of {group}',
    delete: 'Delete mapping of {group}',
    empty: {
      title: 'No mappings for this provider',
      body: 'Without a mapping, users of this provider get a role only through the admin groups or the default role of the configuration.',
    },
    dialog: {
      createTitle: 'Add {provider} group mapping',
      editTitle: 'Edit mapping of {group}',
      description: 'Users in this group receive the role at their next sign in.',
      group: 'Group',
      groupHelperOidc:
        'The full Keycloak group path, for example /rustryx/team-payments/publishers.',
      groupHelperLdap:
        'The full distinguished name, for example cn=platform,ou=groups,dc=example,dc=org.',
      groupRequired: 'Enter the group exactly as the provider reports it.',
      groupTooLong: 'The group is at most 512 characters.',
      groupFixed:
        'The provider and the group cannot change after creation. Delete the mapping and create a new one.',
      warnSpaces:
        'The value starts or ends with a space. Spaces are matched literally, so this only matches a group whose name has them.',
      warnSlash:
        'Keycloak group paths start with /. This value does not, so it will probably match nothing.',
      warnDn:
        'A distinguished name looks like cn=name,ou=groups,dc=example. This value has no = sign.',
      warnUnknown:
        'Rustryx cannot check that the group exists at the provider. A group that does not exist is accepted and never matches.',
      role: 'Role',
      roleHelper:
        'nx-admin and nx-anonymous are not offered: administrators come only from the configured admin groups.',
      roleRequired: 'Choose a role.',
      roleNone: 'Choose a role',
      priorityLabel: 'Priority',
      priorityHelper: 'Lower numbers come first. The roles of every matching mapping apply.',
      priorityInvalid: 'Enter a whole number.',
      save: 'Save mapping',
      create: 'Add mapping',
      created: 'Mapping for {group} added',
      saved: 'Mapping for {group} saved',
      saveFailed: 'The mapping could not be saved',
      duplicate: 'This provider already maps this exact group. Edit that mapping instead.',
    },
    deleteDialog: {
      kind: 'group mapping',
      confirm: 'Delete mapping',
      description:
        'Members of {group} stop receiving the role {role} the next time their groups are read. Nothing else changes. This cannot be undone.',
      deleted: 'Mapping for {group} deleted',
    },
    tester: {
      title: 'Test a group list',
      description:
        'Enter the groups of a user, one per line, to see which roles they would get, including the admin groups and the default role. Nothing is changed.',
      groups: 'Groups',
      groupsHelper: 'One group per line, exactly as the provider reports it.',
      groupsRequired: 'Enter at least one group.',
      run: 'Resolve roles',
      running: 'Resolving',
      resultTitle: 'Resulting roles',
      none: 'No role results.',
      via: {
        admin_group: 'Admin group',
        mapping: 'Mapping',
        default_role: 'Default role',
      },
      unmatched: 'Groups that grant nothing',
      denied: 'The sign in would be refused: no role results and there is no default role.',
      allowed: 'The sign in would be accepted.',
      from: 'from {group}',
    },
  },
} as const

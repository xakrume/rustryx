import { privilegesMessages } from '@/features/privileges/messages'
import { auditMessages } from '@/features/audit/messages'
import { anonymousMessages } from '@/features/anonymous/messages'
import { loginMessages } from '@/features/login/messages'
import { mappingsMessages } from '@/features/idp-mappings/messages'
import { profileMessages } from '@/features/profile/messages'
import { selectorsMessages } from '@/features/selectors/messages'
import { tokensMessages } from '@/features/tokens/messages'
import { usersMessages } from '@/features/users/messages'
import { rolesMessages } from './messages'
import { sharedMessages } from './shared-messages'

/**
 * The message namespaces of the identity and security pages (sign in, profile, tokens, users,
 * roles, privileges, content selectors, group mappings, anonymous access, audit log). Each feature
 * keeps its own file; this one only assembles them, so `i18n/en.ts` spreads a single object.
 */
export const identityMessages = {
  ...sharedMessages,
  ...rolesMessages,
  ...privilegesMessages,
  ...selectorsMessages,
  ...profileMessages,
  ...mappingsMessages,
  ...anonymousMessages,
  ...auditMessages,
  ...loginMessages,
  ...tokensMessages,
  ...usersMessages,
} as const

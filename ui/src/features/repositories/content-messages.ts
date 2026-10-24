import { clientSetupMessages } from '@/features/client-setup/messages'
import { browseMessages } from '@/features/browse/messages'
import { homeMessages } from '@/features/dashboard/messages'
import { uploadMessages } from '@/features/upload/messages'
import { searchMessages } from '@/features/search/messages'
import { componentsMessages } from '@/features/components/messages'
import { repositoriesMessages } from './messages'

/**
 * Every namespace of the content pages (repositories, browse, components, search, upload,
 * client setup). One export, so i18n/en.ts needs a single import and a single spread for the
 * whole group instead of one line per feature that every page agent would edit.
 */
export const contentMessages = {
  ...clientSetupMessages,
  ...repositoriesMessages,
  ...componentsMessages,
  ...browseMessages,
  ...searchMessages,
  ...uploadMessages,
  ...homeMessages,
} as const

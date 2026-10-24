import { blobStoresMessages } from '@/features/blob-stores/messages'
import { aboutPageMessages } from '@/features/system/about-messages'
import { cleanupMessages } from '@/features/cleanup/messages'
import { lifecycleMessages } from '@/features/lifecycle/messages'
import { routingMessages } from '@/features/routing/messages'
import { systemSettingsMessages } from '@/features/settings/messages'
import { systemStatusMessages } from '@/features/system/status-messages'
import { systemWidgetsMessages } from '@/features/system-dashboard/messages'
import { tasksMessages } from '@/features/tasks/messages'
import { runStatusMessages } from '@/features/system/messages'

/**
 * The message namespaces of the system administration pages. They live next to their features
 * and are spread into the catalog (`i18n/en.ts`) by this one object, so the shared catalog file
 * is edited once instead of once per page.
 */
export const adminMessages = {
  aboutPage: aboutPageMessages,
  blobStores: blobStoresMessages,
  cleanup: cleanupMessages,
  lifecycle: lifecycleMessages,
  routing: routingMessages,
  runStatus: runStatusMessages,
  systemSettings: systemSettingsMessages,
  systemStatus: systemStatusMessages,
  systemWidgets: systemWidgetsMessages,
  tasks: tasksMessages,
} as const

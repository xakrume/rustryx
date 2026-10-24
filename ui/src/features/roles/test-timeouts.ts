import { configure } from '@testing-library/react'
import { vi } from 'vitest'

/**
 * The page tests render the whole application. Run in parallel with a coverage report, or on a busy
 * machine, the first render of a route can take longer than the one second Testing Library waits by
 * default and the 15 seconds a test may take, which would fail a correct test now and then. Longer
 * limits cost nothing when the page is ready.
 */
configure({ asyncUtilTimeout: 8000 })
vi.setConfig({ testTimeout: 60_000 })

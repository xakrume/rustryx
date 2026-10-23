import { setupServer } from 'msw/node'

/** The MSW server of the unit tests. It starts EMPTY: a test declares the endpoints it uses. */
export const server = setupServer()

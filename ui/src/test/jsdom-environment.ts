import { builtinEnvironments, type Environment } from 'vitest/environments'

/**
 * The jsdom environment, with Node's own AbortController and AbortSignal kept.
 *
 * Why: jsdom replaces both globals with its own classes, but `fetch` and `Request` are Node's
 * (undici), and undici refuses a signal that is not an instance of ITS AbortSignal
 * ("Expected signal to be an instance of AbortSignal"). TanStack Query passes a signal to
 * every query function, so with jsdom's classes no API call in a test could even start.
 * DOM code in this project never hands an AbortSignal to a jsdom API (addEventListener with
 * `signal`), so keeping Node's classes costs nothing.
 */
const environment: Environment = {
  ...builtinEnvironments.jsdom,
  name: 'jsdom-node-abort',
  async setup(global, options) {
    // `global` is typed any by vitest; this is the one place that touches it.
    const target = global as Record<string, unknown>
    const NodeAbortController = target.AbortController
    const NodeAbortSignal = target.AbortSignal
    const result = await builtinEnvironments.jsdom.setup(global, options)
    target.AbortController = NodeAbortController
    target.AbortSignal = NodeAbortSignal
    return result
  },
}

export default environment

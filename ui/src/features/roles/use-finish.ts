import { useCallback, useEffect, useRef, useState } from 'react'

/**
 * Leaving a form after a successful save, without the unsaved-changes guard stopping it.
 *
 * The guard reads "is the form dirty" from the last render. Calling `form.reset()` and navigating in
 * the same tick leaves the guard with the OLD answer, so it would ask "Discard unsaved changes?"
 * right after the save. Here the form first marks itself `finished` (the guard is given
 * `when={dirty && !finished}`), and `then` runs in the next committed render, when the guard already
 * lets go. `then` may change between renders; the latest one runs.
 */
export function useFinish(then: () => void): { finished: boolean; finish: () => void } {
  const [finished, setFinished] = useState(false)
  const latest = useRef(then)
  useEffect(() => {
    latest.current = then
  })
  useEffect(() => {
    if (finished) latest.current()
  }, [finished])
  const finish = useCallback(() => setFinished(true), [])
  return { finished, finish }
}

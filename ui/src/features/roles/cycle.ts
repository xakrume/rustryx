interface NestedRole {
  id: string
  name: string
  nested_role_ids: readonly string[]
}

/**
 * Would nesting `candidateId` into the role `selfId` form a cycle? It does when the candidate
 * already reaches `selfId` through its own nested roles (a role also cannot hold itself). Returns
 * the chain of role names, from the candidate down to `selfId`, so the picker can say WHY a role
 * is not offered; null when the nesting is fine. The server checks again and answers 409
 * `role_cycle`: this only saves the round trip and explains the cause.
 */
export function cyclePath(
  roles: readonly NestedRole[],
  selfId: string,
  candidateId: string,
): string[] | null {
  const byId = new Map(roles.map((r) => [r.id, r]))
  const start = byId.get(candidateId)
  if (start === undefined) return null
  if (candidateId === selfId) return [start.name]
  const seen = new Set<string>([candidateId])
  const queue: { id: string; path: string[] }[] = [{ id: candidateId, path: [start.name] }]
  while (queue.length > 0) {
    const current = queue.shift()!
    for (const nextId of byId.get(current.id)?.nested_role_ids ?? []) {
      const next = byId.get(nextId)
      if (next === undefined) continue
      const path = [...current.path, next.name]
      if (nextId === selfId) return path
      if (!seen.has(nextId)) {
        seen.add(nextId)
        queue.push({ id: nextId, path })
      }
    }
  }
  return null
}

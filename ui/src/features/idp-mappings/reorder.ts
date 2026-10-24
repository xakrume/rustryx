export interface Orderable {
  id: string
  priority: number
}

export interface PriorityChange {
  id: string
  priority: number
}

/**
 * The priority changes that move the mapping at `index` one place up (-1) or down (+1) in a list
 * ordered by priority. Two different priorities are swapped. Equal priorities (the list then falls
 * back to the group name) cannot be swapped, so the moved mapping steps just past its neighbour.
 * An empty result means there is nowhere to move.
 */
export function priorityChanges(
  rows: readonly Orderable[],
  index: number,
  direction: -1 | 1,
): PriorityChange[] {
  const moved = rows[index]
  const neighbour = rows[index + direction]
  if (moved === undefined || neighbour === undefined) return []
  if (moved.priority !== neighbour.priority) {
    return [
      { id: moved.id, priority: neighbour.priority },
      { id: neighbour.id, priority: moved.priority },
    ]
  }
  return [{ id: moved.id, priority: neighbour.priority + direction }]
}

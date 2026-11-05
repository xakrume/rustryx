import { useRef, useState } from 'react'
import { Tree, type TreeNodeData } from '@/components/tree/tree'
import { DemoSection } from '../demo-section'

const ROOTS: TreeNodeData[] = [
  { id: 'com', label: 'com', hasChildren: true },
  { id: 'org', label: 'org', hasChildren: true },
  { id: 'flaky', label: 'flaky (fails once)', hasChildren: true },
  { id: 'empty', label: 'empty-folder', hasChildren: true },
  { id: 'readme', label: 'README.md', hasChildren: false },
]

function childrenOf(node: TreeNodeData): TreeNodeData[] {
  if (node.id === 'empty') return []
  if (node.id.split('/').length > 3) {
    return [{ id: `${node.id}/lib.jar`, label: 'lib-1.0.0.jar', hasChildren: false }]
  }
  return ['example', 'internal', 'tools']
    .map((name) => ({
      id: `${node.id}/${name}`,
      label: name,
      hasChildren: true,
    }))
    .concat([{ id: `${node.id}/pom.xml`, label: 'pom.xml', hasChildren: false }])
}

export function TreeSection() {
  const [selected, setSelected] = useState<string | null>(null)
  const failedOnce = useRef(false)

  function loadChildren(node: TreeNodeData, signal: AbortSignal): Promise<TreeNodeData[]> {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        if (node.id === 'flaky' && !failedOnce.current) {
          failedOnce.current = true
          reject(new Error('simulated failure'))
        } else resolve(childrenOf(node))
      }, 350)
      signal.addEventListener('abort', () => clearTimeout(timer))
    })
  }

  return (
    <DemoSection
      id="tree"
      title="Tree"
      description="Arrow keys, Home and End, Right and Left to expand and collapse, type a letter to jump. Folders load lazily."
    >
      <div className="grid gap-3 md:grid-cols-2">
        <div className="rounded-xl border border-transparent bg-card dark:border-border/60 p-2">
          <Tree
            label="Repository content"
            roots={ROOTS}
            loadChildren={loadChildren}
            selectedId={selected}
            onSelect={(node) => setSelected(node.id)}
          />
        </div>
        <p className="text-sm text-muted-foreground">
          Selected: <span className="font-mono text-foreground">{selected ?? 'nothing'}</span>
        </p>
      </div>
    </DemoSection>
  )
}

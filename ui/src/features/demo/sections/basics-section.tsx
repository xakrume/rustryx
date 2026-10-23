import { Inbox, Plus, Trash2 } from 'lucide-react'
import { BytesText } from '@/components/bytes-text'
import { EmptyState } from '@/components/empty-state'
import { RelativeTime } from '@/components/relative-time'
import { StatusBadge, type StatusKind } from '@/components/status-badge'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Kbd } from '@/components/ui/kbd'
import { DemoSection } from '../demo-section'

const STATUSES: readonly StatusKind[] = ['ok', 'warning', 'error', 'info', 'neutral', 'unknown']

export function BasicsSection() {
  return (
    <DemoSection
      id="basics"
      title="Buttons, badges, values"
      description="Primary actions use the accent token. Status is always icon plus text."
    >
      <div className="flex flex-wrap items-center gap-2">
        <Button>
          <Plus aria-hidden="true" />
          Primary
        </Button>
        <Button variant="secondary">Secondary</Button>
        <Button variant="outline">Outline</Button>
        <Button variant="ghost">Ghost</Button>
        <Button variant="destructive">
          <Trash2 aria-hidden="true" />
          Destructive
        </Button>
        <Button variant="link">Link</Button>
        <Button loading>Loading</Button>
        <Button disabled>Disabled</Button>
        <Button size="sm">Small</Button>
        <Button size="icon" aria-label="Add">
          <Plus aria-hidden="true" />
        </Button>
      </div>
      <div className="flex flex-wrap items-center gap-2">
        {STATUSES.map((status) => (
          <StatusBadge key={status} status={status} />
        ))}
      </div>
      <Card className="max-w-xl">
        <CardHeader>
          <CardTitle>Values</CardTitle>
        </CardHeader>
        <CardContent className="grid grid-cols-[8rem_1fr] gap-x-4 gap-y-1 text-sm">
          <span className="text-muted-foreground">Size</span>
          <BytesText bytes={8_123_456_789} />
          <span className="text-muted-foreground">Small size</span>
          <BytesText bytes={812} />
          <span className="text-muted-foreground">Unknown size</span>
          <BytesText bytes={null} />
          <span className="text-muted-foreground">Updated</span>
          <RelativeTime value={new Date(Date.now() - 12 * 60_000).toISOString()} />
          <span className="text-muted-foreground">Never</span>
          <RelativeTime value={null} />
          <span className="text-muted-foreground">Shortcut</span>
          <span>
            <Kbd>Ctrl</Kbd> <Kbd>K</Kbd>
          </span>
        </CardContent>
      </Card>
      <div className="grid gap-2 md:grid-cols-2">
        <Alert variant="info" title="Information">
          A neutral note with an icon and text.
        </Alert>
        <Alert variant="success" title="Saved">
          The repository was updated.
        </Alert>
        <Alert variant="warning" title="Warning">
          Anonymous access is enabled.
        </Alert>
        <Alert variant="destructive" title="Failed">
          The blob store is not reachable.
        </Alert>
      </div>
      <EmptyState
        icon={Inbox}
        title="No repositories yet"
        description="Create the first repository to start publishing artifacts."
        action={<Button>Create repository</Button>}
      />
    </DemoSection>
  )
}

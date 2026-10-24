import { CodeBlock } from '@/components/code-block'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { t } from '@/i18n'

/**
 * How to use a token. The value in the example is the literal placeholder `<TOKEN>`: no real
 * secret ever passes through this component, and the page has none to show after creation.
 */
export function TokenUsageHints() {
  const origin = typeof window === 'undefined' ? 'https://rustryx.example' : window.location.origin
  return (
    <Card>
      <CardHeader>
        <CardTitle>{t('tokens.mine.usage.title')}</CardTitle>
        <p className="text-sm text-muted-foreground">{t('tokens.mine.usage.body')}</p>
      </CardHeader>
      <CardContent className="space-y-2">
        <CodeBlock
          label={t('tokens.mine.usage.label')}
          value={`curl -H "Authorization: Bearer <TOKEN>" ${origin}/api/v1/me`}
          wrap
        />
        <p className="text-xs text-muted-foreground">{t('tokens.mine.usage.noteInteractive')}</p>
      </CardContent>
    </Card>
  )
}

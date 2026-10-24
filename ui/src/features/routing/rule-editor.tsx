import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { Link } from '@tanstack/react-router'
import { useState } from 'react'
import { useFormContext } from 'react-hook-form'
import { isApiError } from '@/api/errors'
import { ConfirmDialog } from '@/components/confirm-dialog'
import { ConflictDialog } from '@/components/conflict-dialog'
import { ErrorPanel } from '@/components/error-panel'
import { Form, pointerToName, TextareaField, TextField, useZodForm } from '@/components/form/form'
import { UnsavedChangesGuard } from '@/components/form/unsaved-changes-guard'
import { PageHeader } from '@/components/page-header'
import { Alert } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { Skeleton } from '@/components/ui/skeleton'
import { useSearchPatch } from '@/features/system/search-params'
import { useUpdateResource } from '@/hooks/use-update-resource'
import { t } from '@/i18n'
import { toast } from '@/lib/toast-store'
import {
  createRule,
  deleteRule,
  fetchRule,
  routingKeys,
  ruleQuery,
  updateRule,
  type RoutingRuleInput,
  type RoutingRuleView,
} from './api'
import { PatternRows } from './pattern-rows'
import { EMPTY_RULE, ruleSchema, toInput, valuesFromRule, type RuleFormValues } from './rule-model'
import { RuleTester } from './rule-tester'
import type { UseFormSetError } from 'react-hook-form'

/** `/patterns/2` of the server onto the form's `patterns.2.value`; other pointers keep their name. */
function applyRuleErrors(
  form: { setError: UseFormSetError<RuleFormValues> },
  error: unknown,
): boolean {
  if (!isApiError(error)) return false
  if (error.code === 'already_exists') {
    form.setError('name', { type: 'server', message: error.message }, { shouldFocus: true })
    return true
  }
  const fields = error.fieldErrors
  if (fields.length === 0) return false
  fields.forEach((f, index) => {
    const name = pointerToName(f.field)
    const path = /^patterns\.\d+$/.test(name) ? `${name}.value` : name
    form.setError(
      path as 'name',
      { type: 'server', message: f.message },
      { shouldFocus: index === 0 },
    )
  })
  return true
}

function ModeChoice() {
  const { register, watch } = useFormContext<RuleFormValues>()
  const mode = watch('mode')
  const options = [
    { value: 'block', label: t('routing.mode.blockLabel'), help: t('routing.mode.blockHelp') },
    { value: 'allow', label: t('routing.mode.allowLabel'), help: t('routing.mode.allowHelp') },
  ] as const
  return (
    <fieldset className="space-y-2">
      <legend className="text-sm font-medium">{t('routing.mode.legend')}</legend>
      <div className="grid gap-2 sm:grid-cols-2">
        {options.map((option) => (
          <label
            key={option.value}
            className="flex cursor-pointer items-start gap-2 rounded-lg border border-input bg-card p-3 has-[:checked]:border-2 has-[:checked]:border-foreground has-[:focus-visible]:outline-2 has-[:focus-visible]:outline-offset-2 has-[:focus-visible]:outline-ring"
          >
            <input
              type="radio"
              value={option.value}
              checked={mode === option.value}
              className="mt-1 size-4 accent-accent"
              {...register('mode')}
            />
            <span className="space-y-0.5">
              <span className="block text-sm font-medium">{option.label}</span>
              <span className="block text-xs text-muted-foreground">{option.help}</span>
            </span>
          </label>
        ))}
      </div>
    </fieldset>
  )
}

function Attachments({ rule }: { rule: RoutingRuleView }) {
  return (
    <Card className="max-w-3xl">
      <CardHeader>
        <CardTitle>{t('routing.attachments.title')}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-2">
        {rule.assigned_repositories.length === 0 ? (
          <p className="text-sm text-muted-foreground">{t('routing.attachments.none')}</p>
        ) : (
          <ul className="flex flex-wrap gap-2">
            {rule.assigned_repositories.map((repo) => (
              <li key={repo.id}>
                <Link
                  to="/repositories/$name"
                  params={{ name: repo.name }}
                  className="inline-block rounded-md border border-border bg-muted px-2 py-0.5 font-mono text-xs text-link underline-offset-2 hover:underline"
                >
                  {repo.name}
                </Link>
              </li>
            ))}
          </ul>
        )}
        <p className="text-xs text-muted-foreground">{t('routing.attachments.note')}</p>
      </CardContent>
    </Card>
  )
}

/**
 * Delete is blocked, with the reason and the repositories named, while any repository uses the
 * rule. The contract has no `force` for a routing rule: removing it from the repositories first
 * is the user's decision, never a side effect of this button.
 */
function DeleteRule({ rule, etag }: { rule: RoutingRuleView; etag: string | null }) {
  const [open, setOpen] = useState(false)
  const queryClient = useQueryClient()
  const patchSearch = useSearchPatch()
  const blocked = rule.assigned_repositories.length > 0
  const reasonId = 'rule-delete-reason'
  return (
    <Card className="max-w-3xl">
      <CardHeader>
        <CardTitle>{t('routing.delete.section')}</CardTitle>
      </CardHeader>
      <CardContent className="space-y-3">
        {blocked ? (
          <Alert variant="warning" title={t('routing.delete.blockedTitle')}>
            <span id={reasonId}>
              {t('routing.delete.blocked', {
                count: rule.assigned_repositories.length,
                names: rule.assigned_repositories.map((r) => r.name).join(', '),
              })}
            </span>
          </Alert>
        ) : (
          <p id={reasonId} className="text-sm text-muted-foreground">
            {t('routing.delete.free')}
          </p>
        )}
        <Button
          variant="destructive"
          disabled={blocked}
          aria-describedby={reasonId}
          onClick={() => setOpen(true)}
        >
          {t('common.delete')}
        </Button>
        <ConfirmDialog
          open={open}
          onOpenChange={setOpen}
          kind="routing rule"
          name={rule.name}
          description={t('routing.delete.description')}
          requireTyping
          onConfirm={async () => {
            await deleteRule(rule.id, etag)
            await queryClient.invalidateQueries({ queryKey: routingKeys.all })
            toast.success(t('routing.delete.done', { name: rule.name }))
            patchSearch({ edit: undefined })
          }}
        />
      </CardContent>
    </Card>
  )
}

function RuleForm({ rule, etag }: { rule: RoutingRuleView | undefined; etag: string | null }) {
  const patchSearch = useSearchPatch()
  const queryClient = useQueryClient()
  const form = useZodForm(ruleSchema, { defaultValues: rule ? valuesFromRule(rule) : EMPTY_RULE })

  const create = useMutation({
    mutationFn: (body: RoutingRuleInput) => createRule(body),
    meta: { silent: true },
  })
  const update = useUpdateResource<RoutingRuleView, RoutingRuleInput>({
    queryKey: routingKeys.detail(rule?.id ?? 'new'),
    resourceName: `routing rule ${rule?.name ?? ''}`,
    update: (body, tag) => updateRule(rule?.id ?? '', body, tag),
    fetchCurrent: () => fetchRule(rule?.id ?? ''),
    invalidate: [routingKeys.all],
    copyChanges: (body) => JSON.stringify(body, null, 2),
    onSuccess: (saved) => {
      form.reset(valuesFromRule(saved))
      toast.success(t('routing.toast.saved', { name: saved.name }))
    },
    onError: (error) => {
      if (!applyRuleErrors(form, error)) toast.apiError(error, rule?.name)
    },
  })

  async function onSubmit(values: RuleFormValues): Promise<void> {
    const body = toInput(values)
    if (rule) {
      update.mutate(body)
      return
    }
    try {
      const created = await create.mutateAsync(body)
      form.reset(valuesFromRule(created))
      await queryClient.invalidateQueries({ queryKey: routingKeys.all })
      toast.success(t('routing.toast.created', { name: created.name }))
      patchSearch({ edit: created.id }, { replace: true })
    } catch (error) {
      if (!applyRuleErrors(form, error)) toast.apiError(error)
    }
  }

  const pending = create.isPending || update.isPending
  const isDirty = form.formState.isDirty
  return (
    <div className="space-y-4">
      <UnsavedChangesGuard when={isDirty && !pending} />
      <Form
        form={form}
        onSubmit={onSubmit}
        className="max-w-3xl"
        aria-label={rule ? t('routing.editTitle', { name: rule.name }) : t('routing.createTitle')}
      >
        <div className="sm:max-w-md">
          <TextField<RuleFormValues>
            name="name"
            label={t('routing.fields.name')}
            helper={t('routing.fields.nameHelper')}
            required
            autoComplete="off"
            className="font-mono"
          />
        </div>
        <div className="sm:max-w-xl">
          <TextareaField<RuleFormValues>
            name="description"
            label={t('routing.fields.description')}
            rows={2}
          />
        </div>
        <ModeChoice />
        <PatternRows />
        <div className="flex flex-wrap gap-2">
          <Button type="submit" loading={pending}>
            {pending ? t('routing.saving') : t('routing.save')}
          </Button>
          <Button
            variant="outline"
            disabled={!isDirty || pending}
            onClick={() => form.reset(rule ? valuesFromRule(rule) : EMPTY_RULE)}
          >
            {t('routing.discard')}
          </Button>
        </div>
        <RuleTester />
      </Form>
      <ConflictDialog {...update.conflict} />
      {rule ? (
        <>
          <Attachments rule={rule} />
          <DeleteRule rule={rule} etag={etag} />
        </>
      ) : null}
    </div>
  )
}

/** `/admin/routing-rules?edit=<id|new>`: the editor of one rule with its path tester. */
export function RuleEditorView({ ruleId }: { ruleId: string }) {
  const patchSearch = useSearchPatch()
  const isNew = ruleId === 'new'
  const query = useQuery({ ...ruleQuery(ruleId), enabled: !isNew })
  const stored = query.data
  return (
    <>
      <PageHeader
        title={
          isNew
            ? t('routing.createTitle')
            : stored
              ? t('routing.editTitle', { name: stored.data.name })
              : t('routing.title')
        }
        description={t('routing.description')}
        actions={
          <Button variant="outline" onClick={() => patchSearch({ edit: undefined })}>
            {t('routing.backToList')}
          </Button>
        }
      />
      {!isNew && query.isError && !stored ? (
        <ErrorPanel error={query.error} onRetry={() => void query.refetch()} />
      ) : !isNew && !stored ? (
        <div aria-busy="true" className="space-y-3">
          <Skeleton className="h-10 max-w-md" />
          <Skeleton className="h-48 max-w-3xl" />
        </div>
      ) : (
        <RuleForm key={stored?.etag ?? 'new'} rule={stored?.data} etag={stored?.etag ?? null} />
      )}
    </>
  )
}

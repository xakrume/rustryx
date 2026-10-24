import { UploadCloud } from 'lucide-react'
import { useId, useState } from 'react'
import { t } from '@/i18n'
import { cn } from '@/lib/utils'

export interface FileDropProps {
  multiple: boolean
  /** Comma separated extensions for the file picker, for example ".jar,.pom". */
  accept: string
  hint: string
  disabled?: boolean
  onFiles: (files: File[]) => void
}

/**
 * Drop files on the area or choose them. The control is a real `<input type="file">` (hidden
 * visually, still focusable) with the drop area as its label, so the keyboard works without a
 * line of script: Tab to it, Enter or Space opens the file dialog, and the focus ring shows on
 * the area. Dragging is an extra for pointer users, never the only way.
 */
export function FileDrop({ multiple, accept, hint, disabled = false, onFiles }: FileDropProps) {
  const id = useId()
  const [dragging, setDragging] = useState(false)
  return (
    <div
      onDragOver={(event) => {
        if (disabled) return
        event.preventDefault()
        setDragging(true)
      }}
      onDragLeave={() => setDragging(false)}
      onDrop={(event) => {
        event.preventDefault()
        setDragging(false)
        if (!disabled) onFiles(Array.from(event.dataTransfer.files))
      }}
    >
      <input
        id={id}
        type="file"
        multiple={multiple}
        accept={accept === '' ? undefined : accept}
        disabled={disabled}
        className="peer sr-only"
        onChange={(event) => {
          onFiles(Array.from(event.target.files ?? []))
          // Choosing the same file again must fire a change event again.
          event.target.value = ''
        }}
      />
      <label
        htmlFor={id}
        className={cn(
          'flex cursor-pointer flex-col items-center gap-1 rounded-lg border-2 border-dashed border-input bg-card px-4 py-8 text-center transition-colors duration-150 hover:bg-muted peer-focus-visible:outline-2 peer-focus-visible:outline-offset-2 peer-focus-visible:outline-ring',
          dragging && 'border-accent bg-muted',
          disabled && 'cursor-not-allowed opacity-60',
        )}
      >
        <UploadCloud className="size-8 text-muted-foreground" aria-hidden="true" />
        <span className="font-medium">
          {multiple ? t('upload.chooseFiles') : t('upload.chooseFile')}
          <span className="font-normal text-muted-foreground"> {t('upload.orDrop')}</span>
        </span>
        <span className="text-xs text-muted-foreground">{hint}</span>
      </label>
    </div>
  )
}

/**
 * A confirmation dialog.
 *
 * # Why the confirm button names the action
 *
 * "OK" and "Confirm" are the labels a reader has learned to click without reading.
 * Naming what the button will do — "Stop", "Close all", "Roll back" — is what makes
 * the dialog have to be read, and it is why this takes a label rather than
 * rendering one itself.
 *
 * # Why the variant is destructive and not configurable
 *
 * Every confirmation in this interface guards something that interrupts traffic or
 * discards state. A non-destructive confirmation would be a speed bump on an
 * ordinary action, and the right answer there is no dialog at all.
 */

import type { ReactNode } from 'react'

import { Button } from '@/components/ui/button'
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from '@/components/ui/dialog'

export function ConfirmDialog({
  open,
  onOpenChange,
  title,
  description,
  confirmLabel,
  cancelLabel,
  pending,
  onConfirm,
  onCancel,
  children,
}: {
  open: boolean
  onOpenChange: (open: boolean) => void
  title: string
  description: string
  confirmLabel: string
  cancelLabel: string
  pending?: boolean
  onConfirm: () => void
  onCancel?: () => void
  /** Extra content, such as an input the action needs. */
  children?: ReactNode
}) {
  return (
    <Dialog open={open} onOpenChange={onOpenChange}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>{title}</DialogTitle>
          <DialogDescription>{description}</DialogDescription>
        </DialogHeader>
        {children}
        <DialogFooter>
          <Button
            variant="outline"
            onClick={() => {
              onCancel?.()
              onOpenChange(false)
            }}
            disabled={pending}
          >
            {cancelLabel}
          </Button>
          <Button variant="destructive" onClick={onConfirm} disabled={pending}>
            {confirmLabel}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}

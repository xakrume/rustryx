import { render, screen, waitFor } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { useState } from 'react'
import { describe, expect, it } from 'vitest'
import { expectNoA11yViolations } from '@/test/utils'
import {
  Dialog,
  DialogClose,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
  DialogTrigger,
} from './dialog'

function Example({ dismissible = true }: { dismissible?: boolean }) {
  return (
    <div id="app">
      <Dialog>
        <DialogTrigger>Open dialog</DialogTrigger>
        <DialogContent dismissible={dismissible}>
          <DialogHeader>
            <DialogTitle>Rename repository</DialogTitle>
            <DialogDescription>Choose a new display name.</DialogDescription>
          </DialogHeader>
          <input aria-label="Name" />
          <DialogFooter>
            <DialogClose>Cancel</DialogClose>
            <button type="button">Save</button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      <button type="button">Outside button</button>
    </div>
  )
}

describe('Dialog', () => {
  it('opens with a name and a description, and has no accessibility violations', async () => {
    const user = userEvent.setup()
    render(<Example />)
    await user.click(screen.getByRole('button', { name: 'Open dialog' }))
    const dialog = await screen.findByRole('dialog', { name: 'Rename repository' })
    expect(dialog).toHaveAccessibleDescription('Choose a new display name.')
    await expectNoA11yViolations(document.body)
  })

  it('moves focus inside, traps Tab, and hides the rest of the page from assistive tech', async () => {
    const user = userEvent.setup()
    const { container } = render(<Example />)
    await user.click(screen.getByRole('button', { name: 'Open dialog' }))
    const dialog = await screen.findByRole('dialog')
    expect(dialog.contains(document.activeElement)).toBe(true)
    // Everything outside the portalled dialog is aria-hidden (here: the render container).
    expect(container).toHaveAttribute('aria-hidden', 'true')
    // Tab never leaves the dialog, however many times it is pressed.
    for (let i = 0; i < 8; i += 1) {
      await user.tab()
      expect(dialog.contains(document.activeElement)).toBe(true)
    }
  })

  it('closes on Escape and returns focus to the control that opened it', async () => {
    const user = userEvent.setup()
    const { container } = render(<Example />)
    const trigger = screen.getByRole('button', { name: 'Open dialog' })
    await user.click(trigger)
    await screen.findByRole('dialog')
    await user.keyboard('{Escape}')
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
    expect(trigger).toHaveFocus()
    expect(container).not.toHaveAttribute('aria-hidden')
  })

  it('closes on the Cancel button and on a click outside', async () => {
    const user = userEvent.setup()
    render(<Example />)
    await user.click(screen.getByRole('button', { name: 'Open dialog' }))
    await user.click(await screen.findByRole('button', { name: 'Cancel' }))
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())

    await user.click(screen.getByRole('button', { name: 'Open dialog' }))
    await screen.findByRole('dialog')
    await user.click(
      document.querySelector('[aria-hidden="true"][data-state="open"]') as HTMLElement,
    )
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
  })

  it('must_not_close_on_escape_or_outside_click_when_not_dismissible', async () => {
    const user = userEvent.setup()
    render(<Example dismissible={false} />)
    await user.click(screen.getByRole('button', { name: 'Open dialog' }))
    await screen.findByRole('dialog')
    await user.keyboard('{Escape}')
    await user.click(
      document.querySelector('[aria-hidden="true"][data-state="open"]') as HTMLElement,
    )
    expect(screen.getByRole('dialog')).toBeInTheDocument()
    // No corner close button either: the footer has to offer the way out.
    expect(screen.queryByRole('button', { name: 'Close' })).not.toBeInTheDocument()
  })

  it('must_not_inject_a_style_element (the production CSP has no unsafe-inline)', async () => {
    const user = userEvent.setup()
    const before = document.querySelectorAll('style').length
    render(<Example />)
    await user.click(screen.getByRole('button', { name: 'Open dialog' }))
    await screen.findByRole('dialog')
    expect(document.querySelectorAll('style').length).toBe(before)
    // Scroll lock is a CSS rule keyed on this attribute, not injected CSS.
    expect(document.querySelector('[data-dialog-open]')).not.toBeNull()
  })

  it('works controlled', async () => {
    function Controlled() {
      const [open, setOpen] = useState(true)
      return (
        <Dialog open={open} onOpenChange={setOpen}>
          <DialogContent>
            <DialogTitle>Controlled</DialogTitle>
            <DialogDescription>Body</DialogDescription>
          </DialogContent>
        </Dialog>
      )
    }
    const user = userEvent.setup()
    render(<Controlled />)
    expect(await screen.findByRole('dialog', { name: 'Controlled' })).toBeInTheDocument()
    await user.keyboard('{Escape}')
    await waitFor(() => expect(screen.queryByRole('dialog')).not.toBeInTheDocument())
  })
})

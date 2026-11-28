import { cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { Thread, ThreadMessage } from '#/domain/tasks/ui/task-thread'

afterEach(cleanup)

const speaker = { kind: 'human', name: 'Moi', mine: true } as const
const at = '2026-11-01T10:00:00Z'

const message = (props: Partial<Parameters<typeof ThreadMessage>[0]> = {}) =>
	render(
		<Thread>
			<ThreadMessage speaker={speaker} body="Bonjour" at={at} {...props} />
		</Thread>,
	)

describe('ThreadMessage delivery', () => {
	it('dims a pending message and says it is being sent', () => {
		message({ delivery: 'pending' })
		expect(screen.getByText('Envoi…')).toBeTruthy()
		expect(screen.getByRole('listitem').className).toContain('opacity-60')
	})

	it('offers to retry or discard a failed message', () => {
		const onRetry = vi.fn()
		const onDiscard = vi.fn()
		message({ delivery: 'failed', queued: true, onRetry, onDiscard })
		expect(screen.getByText('Non envoyé')).toBeTruthy()
		expect(screen.queryByText(/En file/)).toBeNull()
		fireEvent.click(screen.getByRole('button', { name: 'Réessayer' }))
		fireEvent.click(screen.getByRole('button', { name: 'Supprimer' }))
		expect(onRetry).toHaveBeenCalledOnce()
		expect(onDiscard).toHaveBeenCalledOnce()
	})

	it('marks a queued message', () => {
		message({ queued: true })
		expect(
			screen.getByText("En file : l'agent le lira à la fin de son tour"),
		).toBeTruthy()
	})

	it('shows nothing extra for a sent message', () => {
		message()
		expect(screen.queryByText('Envoi…')).toBeNull()
		expect(screen.queryByText(/En file/)).toBeNull()
		expect(screen.queryByRole('button', { name: 'Réessayer' })).toBeNull()
	})
})

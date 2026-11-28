import {
	cleanup,
	fireEvent,
	render,
	screen,
	within,
} from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import type { RunStreamEvent } from '#/domain/tasks/run-stream'
import { AgentTurn, StopButton, TurnHeader } from '#/domain/tasks/ui/agent-turn'

afterEach(cleanup)

const event = (
	seq: number,
	kind: RunStreamEvent['kind'],
	payload: Record<string, unknown>,
): RunStreamEvent => ({ seq, kind, payload, createdAt: '2026-11-01T00:00:00Z' })

describe('AgentTurn', () => {
	it('renders events in order with the live text last', () => {
		render(
			<AgentTurn
				active
				events={[
					event(1, 'assistant_text', { text: 'Premier', truncated: false }),
					event(2, 'tool_use', { id: 't', tool: 'lire', input: { p: 'a' } }),
					event(3, 'tool_result', { id: 't', succeeded: true, output: 'ok' }),
					event(4, 'system', { text: 'Reprise' }),
				]}
				live="En cours d'écriture"
			/>,
		)
		const region = screen.getByRole('region', { name: "Exécution de l'agent" })
		expect(region.getAttribute('aria-busy')).toBe('true')
		expect(region.getAttribute('aria-live')).toBe('polite')
		const text = region.textContent ?? ''
		expect(text.indexOf('Premier')).toBeLessThan(text.indexOf('lire'))
		expect(text.indexOf('lire')).toBeLessThan(text.indexOf('Reprise'))
		expect(text.indexOf('Reprise')).toBeLessThan(
			text.indexOf("En cours d'écriture"),
		)
		expect(
			within(region).getAllByRole('button', { name: /lire/ }),
		).toHaveLength(1)
	})

	it('does not announce the live text', () => {
		render(<AgentTurn active events={[]} live="jeton" />)
		expect(
			screen
				.getByText('jeton')
				.closest('[aria-live]')
				?.getAttribute('aria-live'),
		).toBe('off')
	})

	it('pairs a call with its result in one collapsible row', () => {
		render(
			<AgentTurn
				active={false}
				events={[
					event(1, 'tool_use', {
						id: 't',
						tool: 'lire',
						input: { chemin: 'a.txt' },
					}),
					event(2, 'tool_result', {
						id: 't',
						succeeded: false,
						output: 'introuvable',
					}),
				]}
				live=""
			/>,
		)
		const row = screen.getByRole('button', { name: /lire/ })
		expect(row.getAttribute('aria-expanded')).toBe('false')
		expect(screen.getByText(/a\.txt/)).toBeTruthy()
		expect(screen.getByText('Échec')).toBeTruthy()
		expect(screen.queryByText('introuvable')).toBeNull()
		fireEvent.click(row)
		expect(row.getAttribute('aria-expanded')).toBe('true')
		expect(screen.getByText('introuvable')).toBeTruthy()
	})

	it('folds a long output until asked', () => {
		const output = Array.from({ length: 40 }, (_, i) => `ligne-${i}`).join('\n')
		render(
			<AgentTurn
				active={false}
				events={[
					event(1, 'tool_use', { id: 't', tool: 'lister', input: {} }),
					event(2, 'tool_result', { id: 't', succeeded: true, output }),
				]}
				live=""
			/>,
		)
		fireEvent.click(screen.getByRole('button', { name: /lister/ }))
		expect(screen.queryByText(/ligne-39/)).toBeNull()
		fireEvent.click(screen.getByRole('button', { name: 'Afficher tout' }))
		expect(screen.getByText(/ligne-39/)).toBeTruthy()
		fireEvent.click(screen.getByRole('button', { name: 'Réduire' }))
		expect(screen.queryByText(/ligne-39/)).toBeNull()
	})

	it('says when a text was truncated', () => {
		render(
			<AgentTurn
				active={false}
				events={[
					event(1, 'assistant_text', { text: 'Coupé', truncated: true }),
				]}
				live=""
			/>,
		)
		expect(screen.getByText('texte tronqué')).toBeTruthy()
	})

	it('is not busy once finished', () => {
		render(<AgentTurn active={false} events={[]} live="" />)
		expect(
			screen
				.getByRole('region', { name: "Exécution de l'agent" })
				.getAttribute('aria-busy'),
		).toBe('false')
	})
})

describe('TurnHeader', () => {
	it('shows the agent, the status and the writing state', () => {
		render(
			<TurnHeader
				agent={{ name: 'Lead', hue: 10 }}
				status="En cours"
				active
				actions={<span>action</span>}
			/>,
		)
		expect(screen.getByText('Lead')).toBeTruthy()
		expect(screen.getByText('En cours')).toBeTruthy()
		expect(screen.getByText('écrit…')).toBeTruthy()
		expect(screen.getByText('action')).toBeTruthy()
	})
})

describe('StopButton', () => {
	it('stops on click', () => {
		const onStop = vi.fn()
		render(<StopButton cancelling={false} onStop={onStop} />)
		const button = screen.getByRole('button', { name: 'Arrêter' })
		expect(button.getAttribute('aria-keyshortcuts')).toBe('Control+. Meta+.')
		fireEvent.click(button)
		expect(onStop).toHaveBeenCalledOnce()
	})

	it('is disabled and explains while cancelling', () => {
		const onStop = vi.fn()
		render(<StopButton cancelling onStop={onStop} />)
		const button = screen.getByRole('button', { name: 'Arrêt en cours…' })
		expect((button as HTMLButtonElement).disabled).toBe(true)
		fireEvent.click(button)
		expect(onStop).not.toHaveBeenCalled()
	})
})

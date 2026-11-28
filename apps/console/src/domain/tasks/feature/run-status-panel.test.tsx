import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { cleanup, render, screen } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { RunStatusPanel } from '#/domain/tasks/feature/run-status-panel'
import { useRunPolling } from '#/domain/tasks/hooks/use-agent-run'
import { realtimeStore } from '#/lib/realtime'

vi.mock('#/domain/tasks/hooks/use-agent-run', () => ({
	useRunPolling: vi.fn(),
	useRefreshCard: () => vi.fn(),
}))

vi.mock('#/domain/tasks/hooks/use-run-stream', () => ({
	useRunStream: () => ({
		events: [],
		live: 'texte',
		connection: 'live',
		loading: false,
	}),
}))

afterEach(() => {
	cleanup()
	realtimeStore.setState(() => ({ state: 'offline' }))
	vi.mocked(useRunPolling).mockReset()
})

function show(status: string) {
	vi.mocked(useRunPolling).mockReturnValue({
		data: { id: 'r', status },
		isError: false,
	} as never)
	const onStop = vi.fn()
	render(
		<QueryClientProvider client={new QueryClient()}>
			<RunStatusPanel
				runId="r"
				agent={{ name: 'Lead', hue: 1 }}
				onActiveChange={vi.fn()}
				onStop={onStop}
				stopNotice={null}
			/>
		</QueryClientProvider>,
	)
}

describe('RunStatusPanel polling', () => {
	it('polls the run when realtime is not connected', () => {
		show('running')
		expect(useRunPolling).toHaveBeenCalledWith('r', true)
		expect(screen.getByText('texte')).toBeTruthy()
	})

	it('does not poll the run when realtime is connected', () => {
		realtimeStore.setState(() => ({ state: 'connected' }))
		show('queued')
		expect(useRunPolling).toHaveBeenCalledWith('r', false)
		expect(screen.getByText('En file d’attente')).toBeTruthy()
	})
})

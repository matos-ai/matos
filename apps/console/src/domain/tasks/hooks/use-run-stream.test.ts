import { act, renderHook } from '@testing-library/react'
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { useRunStream } from '#/domain/tasks/hooks/use-run-stream'
import {
	type RunSubscriptionHandlers,
	realtimeStore,
	subscribeRun,
} from '#/lib/realtime'

const getMock = vi.hoisted(() => vi.fn())

vi.mock('#/api', () => ({ api: { get: getMock } }))

vi.mock('#/lib/realtime', async (importOriginal) => ({
	...(await importOriginal<typeof import('#/lib/realtime')>()),
	subscribeRun: vi.fn(),
}))

function raw(seq: number, text = `m${seq}`) {
	return {
		seq,
		kind: 'assistant_text',
		payload: { text, truncated: false },
		created_at: '2026-01-01T00:00:00Z',
	}
}

function page(...seqs: number[]) {
	return { events: seqs.map((seq) => raw(seq)), next_after: null }
}

let handlers: RunSubscriptionHandlers
let leave: ReturnType<typeof vi.fn>

async function settle(ms = 20) {
	await act(async () => {
		await vi.advanceTimersByTimeAsync(ms)
	})
}

describe('useRunStream', () => {
	beforeEach(() => {
		vi.useFakeTimers()
		getMock.mockReset()
		leave = vi.fn()
		vi.mocked(subscribeRun).mockImplementation((_runId, next) => {
			handlers = next
			return leave as () => void
		})
		realtimeStore.setState(() => ({ state: 'connected' }))
	})

	afterEach(() => {
		vi.useRealTimers()
		realtimeStore.setState(() => ({ state: 'offline' }))
	})

	it('is empty and idle without a run', async () => {
		const { result } = renderHook(() => useRunStream(null, true))
		await settle()
		expect(result.current).toEqual({
			events: [],
			live: '',
			connection: 'idle',
			loading: false,
		})
		expect(getMock).not.toHaveBeenCalled()
		expect(subscribeRun).not.toHaveBeenCalled()
	})

	it('loads the stored history of a finished run without subscribing', async () => {
		getMock.mockResolvedValue(page(1, 2))
		const { result } = renderHook(() => useRunStream('r1', false))
		expect(result.current.loading).toBe(true)
		await settle()
		expect(result.current.loading).toBe(false)
		expect(result.current.events.map((e) => e.seq)).toEqual([1, 2])
		expect(result.current.events[0]).toEqual({
			seq: 1,
			kind: 'assistant_text',
			payload: { text: 'm1', truncated: false },
			createdAt: '2026-01-01T00:00:00Z',
		})
		expect(result.current.connection).toBe('idle')
		expect(getMock).toHaveBeenCalledWith('/api/v1/runs/{run_id}/events', {
			path: { run_id: 'r1' },
			query: { after: 0, limit: 1000 },
		})
		expect(subscribeRun).not.toHaveBeenCalled()
	})

	it('shows live deltas then replaces them with the stored event', async () => {
		getMock.mockResolvedValueOnce(page(1))
		const { result } = renderHook(() => useRunStream('r1', true))
		await settle()
		act(() => handlers.onSubscribed())
		await settle()
		expect(result.current.connection).toBe('live')
		act(() => {
			handlers.onDelta(1, 'Hel')
			handlers.onDelta(1, 'lo')
		})
		await settle()
		expect(result.current.live).toBe('Hello')
		getMock.mockResolvedValueOnce(page(2))
		act(() => handlers.onEvents(2))
		await settle()
		expect(result.current.events.map((e) => e.seq)).toEqual([1, 2])
		expect(result.current.live).toBe('')
		expect(getMock).toHaveBeenLastCalledWith('/api/v1/runs/{run_id}/events', {
			path: { run_id: 'r1' },
			query: { after: 1, limit: 1000 },
		})
	})

	it('batches several deltas into one render per frame', async () => {
		getMock.mockResolvedValue(page())
		let renders = 0
		const { result } = renderHook(() => {
			renders += 1
			return useRunStream('r1', true)
		})
		await settle()
		act(() => handlers.onSubscribed())
		await settle()
		const before = renders
		act(() => {
			for (const piece of ['a', 'b', 'c', 'd']) handlers.onDelta(0, piece)
		})
		await settle()
		expect(result.current.live).toBe('abcd')
		expect(renders - before).toBe(1)
	})

	it('polls the events when realtime is offline', async () => {
		realtimeStore.setState(() => ({ state: 'offline' }))
		getMock.mockResolvedValueOnce(page(1))
		const { result } = renderHook(() => useRunStream('r1', true))
		await settle()
		expect(result.current.connection).toBe('polling')
		getMock.mockResolvedValueOnce(page(2))
		await settle(2000)
		expect(result.current.events.map((e) => e.seq)).toEqual([1, 2])
		expect(getMock).toHaveBeenLastCalledWith('/api/v1/runs/{run_id}/events', {
			path: { run_id: 'r1' },
			query: { after: 1, limit: 1000 },
		})
	})

	it('falls back to polling when the subscription is refused', async () => {
		getMock.mockResolvedValue(page())
		const { result } = renderHook(() => useRunStream('r1', true))
		await settle()
		act(() => handlers.onRefused())
		await settle()
		expect(result.current.connection).toBe('polling')
	})

	it('falls back to polling when subscribed never arrives', async () => {
		getMock.mockResolvedValue(page())
		const { result } = renderHook(() => useRunStream('r1', true))
		await settle(2900)
		expect(result.current.connection).toBe('idle')
		await settle(200)
		expect(result.current.connection).toBe('polling')
	})

	it('refetches from the last contiguous seq when a gap appears', async () => {
		getMock.mockResolvedValueOnce(page(1, 3))
		getMock.mockResolvedValueOnce(page(2, 3))
		const { result } = renderHook(() => useRunStream('r1', false))
		await settle()
		expect(result.current.events.map((e) => e.seq)).toEqual([1, 2, 3])
		expect(getMock).toHaveBeenLastCalledWith('/api/v1/runs/{run_id}/events', {
			path: { run_id: 'r1' },
			query: { after: 1, limit: 1000 },
		})
	})

	it('refetches on resync', async () => {
		getMock.mockResolvedValueOnce(page(1))
		renderHook(() => useRunStream('r1', true))
		await settle()
		getMock.mockResolvedValueOnce(page(2))
		act(() => handlers.onResync())
		await settle()
		expect(getMock).toHaveBeenCalledTimes(2)
	})

	it('does a final fetch, unsubscribes and keeps the events when the run stops', async () => {
		getMock.mockResolvedValueOnce(page(1))
		const { result, rerender } = renderHook(
			({ active }) => useRunStream('r1', active),
			{ initialProps: { active: true } },
		)
		await settle()
		act(() => handlers.onSubscribed())
		await settle()
		act(() => handlers.onDelta(1, 'tail'))
		await settle()
		expect(result.current.live).toBe('tail')
		getMock.mockResolvedValueOnce(page(2))
		rerender({ active: false })
		await settle()
		expect(leave).toHaveBeenCalledTimes(1)
		expect(result.current.events.map((e) => e.seq)).toEqual([1, 2])
		expect(result.current.live).toBe('')
		expect(result.current.connection).toBe('idle')
	})

	it('unsubscribes on unmount', async () => {
		getMock.mockResolvedValue(page())
		const { unmount } = renderHook(() => useRunStream('r1', true))
		await settle()
		unmount()
		expect(leave).toHaveBeenCalledTimes(1)
	})
})

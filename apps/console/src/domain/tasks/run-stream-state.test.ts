import { describe, expect, it } from 'vitest'
import type { RunStreamEvent } from '#/domain/tasks/run-stream'
import {
	initialRunStreamState,
	type RunStreamAction,
	runStreamReducer,
} from '#/domain/tasks/run-stream-state'

function event(seq: number): RunStreamEvent {
	return {
		seq,
		kind: 'assistant_text',
		payload: { text: `m${seq}`, truncated: false },
		createdAt: '2026-01-01T00:00:00Z',
	}
}

function run(...actions: RunStreamAction[]) {
	return actions.reduce(runStreamReducer, initialRunStreamState)
}

describe('runStreamReducer', () => {
	it('stores ordered events and advances the last seq', () => {
		const state = run({ type: 'events', events: [event(1), event(2)] })
		expect(state.events.map((e) => e.seq)).toEqual([1, 2])
		expect(state.lastSeq).toBe(2)
		expect(state.needsFetch).toBe(false)
	})

	it('sorts out-of-order events and ignores duplicates', () => {
		const state = run(
			{ type: 'events', events: [event(2), event(1)] },
			{ type: 'events', events: [event(2), event(3)] },
		)
		expect(state.events.map((e) => e.seq)).toEqual([1, 2, 3])
		expect(state.lastSeq).toBe(3)
	})

	it('returns the same state for a batch of known events', () => {
		const state = run({ type: 'events', events: [event(1)] })
		expect(
			runStreamReducer(state, { type: 'events', events: [event(1)] }),
		).toBe(state)
	})

	it('flags a gap and keeps the last contiguous seq', () => {
		const state = run({ type: 'events', events: [event(1), event(3)] })
		expect(state.lastSeq).toBe(1)
		expect(state.needsFetch).toBe(true)
		const filled = runStreamReducer(
			runStreamReducer(state, { type: 'fetching' }),
			{ type: 'events', events: [event(2)] },
		)
		expect(filled.lastSeq).toBe(3)
		expect(filled.needsFetch).toBe(false)
	})

	it('appends a delta only when after equals the last seq', () => {
		const state = run(
			{ type: 'events', events: [event(1)] },
			{ type: 'delta', after: 1, text: 'Hel' },
			{ type: 'delta', after: 1, text: 'lo' },
		)
		expect(state.live).toBe('Hello')
		expect(state.needsFetch).toBe(false)
	})

	it('ignores a stale delta', () => {
		const base = run({ type: 'events', events: [event(1), event(2)] })
		const state = runStreamReducer(base, {
			type: 'delta',
			after: 1,
			text: 'old',
		})
		expect(state).toBe(base)
	})

	it('requests a fetch for a delta ahead of the stored events', () => {
		const state = run(
			{ type: 'events', events: [event(1)] },
			{ type: 'delta', after: 3, text: 'x' },
		)
		expect(state.needsFetch).toBe(true)
		const caught = run(
			{ type: 'events', events: [event(1)] },
			{ type: 'delta', after: 3, text: 'x' },
			{ type: 'fetching' },
			{ type: 'events', events: [event(2), event(3)] },
			{ type: 'delta', after: 3, text: 'y' },
		)
		expect(caught.live).toBe('xy')
		expect(caught.needsFetch).toBe(false)
	})

	it('starts a new live text when a delta carries a new after', () => {
		const state = run(
			{ type: 'delta', after: 0, text: 'a' },
			{ type: 'events', events: [event(1)] },
			{ type: 'delta', after: 1, text: 'b' },
		)
		expect(state.live).toBe('b')
	})

	it('drops the live text once stored events pass after', () => {
		const state = run(
			{ type: 'delta', after: 0, text: 'typing' },
			{ type: 'events', events: [event(1)] },
		)
		expect(state.live).toBe('')
		expect(state.liveAfter).toBeNull()
	})

	it('keeps the live text while stored events have not passed after', () => {
		const state = run(
			{ type: 'events', events: [event(1)] },
			{ type: 'delta', after: 1, text: 'typing' },
			{ type: 'events', events: [event(1)] },
		)
		expect(state.live).toBe('typing')
	})

	it('asks for a fetch when events were stored beyond the last seq', () => {
		const state = run(
			{ type: 'events', events: [event(1)] },
			{ type: 'stored', last: 2 },
		)
		expect(state.needsFetch).toBe(true)
		const same = run(
			{ type: 'events', events: [event(1)] },
			{ type: 'stored', last: 1 },
		)
		expect(same.needsFetch).toBe(false)
	})

	it('asks for a fetch on resync and clears it when fetching', () => {
		const state = run({ type: 'resync' })
		expect(state.needsFetch).toBe(true)
		expect(runStreamReducer(state, { type: 'fetching' }).needsFetch).toBe(false)
	})

	it('clears the live text on demand', () => {
		const state = run(
			{ type: 'delta', after: 0, text: 'x' },
			{ type: 'clearLive' },
		)
		expect(state.live).toBe('')
	})
})

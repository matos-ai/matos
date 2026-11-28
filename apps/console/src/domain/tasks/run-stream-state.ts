import type { RunStreamEvent } from '#/domain/tasks/run-stream'

export interface RunStreamState {
	events: RunStreamEvent[]
	live: string
	liveAfter: number | null
	lastSeq: number
	needsFetch: boolean
}

export type RunStreamAction =
	| { type: 'events'; events: RunStreamEvent[] }
	| { type: 'delta'; after: number; text: string }
	| { type: 'stored'; last: number }
	| { type: 'resync' }
	| { type: 'fetching' }
	| { type: 'clearLive' }

export const initialRunStreamState: RunStreamState = {
	events: [],
	live: '',
	liveAfter: null,
	lastSeq: 0,
	needsFetch: false,
}

function dropStaleLive(state: RunStreamState): RunStreamState {
	if (state.liveAfter !== null && state.lastSeq > state.liveAfter) {
		return { ...state, live: '', liveAfter: null }
	}
	return state
}

function mergeEvents(
	state: RunStreamState,
	incoming: RunStreamEvent[],
): RunStreamState {
	const known = new Set(state.events.map((event) => event.seq))
	const added: RunStreamEvent[] = []
	for (const event of incoming) {
		if (known.has(event.seq)) continue
		known.add(event.seq)
		added.push(event)
	}
	if (added.length === 0) return state
	const events = [...state.events, ...added].sort((a, b) => a.seq - b.seq)
	let lastSeq = state.lastSeq
	for (const event of events) {
		if (event.seq === lastSeq + 1) lastSeq = event.seq
	}
	const gap = events.some((event) => event.seq > lastSeq + 1)
	return dropStaleLive({
		...state,
		events,
		lastSeq,
		needsFetch: state.needsFetch || gap,
	})
}

export function runStreamReducer(
	state: RunStreamState,
	action: RunStreamAction,
): RunStreamState {
	switch (action.type) {
		case 'events':
			return mergeEvents(state, action.events)
		case 'delta': {
			if (action.after < state.lastSeq) return state
			const live =
				state.liveAfter === action.after
					? state.live + action.text
					: action.text
			return {
				...state,
				live,
				liveAfter: action.after,
				needsFetch: state.needsFetch || action.after > state.lastSeq,
			}
		}
		case 'stored':
			return action.last > state.lastSeq
				? { ...state, needsFetch: true }
				: state
		case 'resync':
			return { ...state, needsFetch: true }
		case 'fetching':
			return state.needsFetch ? { ...state, needsFetch: false } : state
		case 'clearLive':
			return state.live === '' && state.liveAfter === null
				? state
				: { ...state, live: '', liveAfter: null }
	}
}

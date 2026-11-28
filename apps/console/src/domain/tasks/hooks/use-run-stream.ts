import { useEffect, useRef, useState } from 'react'
import { api } from '#/api'
import type { Schemas } from '#/api/api.client'
import type {
	RunEventKind,
	RunStream,
	RunStreamConnection,
	RunStreamEvent,
} from '#/domain/tasks/run-stream'
import {
	initialRunStreamState,
	type RunStreamAction,
	type RunStreamState,
	runStreamReducer,
} from '#/domain/tasks/run-stream-state'
import { realtimeStore, subscribeRun } from '#/lib/realtime'

export const RUN_STREAM_PAGE = 1000
export const RUN_STREAM_POLL_MS = 2000
export const RUN_STREAM_SUBSCRIBE_TIMEOUT_MS = 3000
const FRAME_FALLBACK_MS = 16

interface View {
	runId: string
	state: RunStreamState
}

function toEvent(raw: Schemas.RunEventResponse): RunStreamEvent {
	const payload =
		typeof raw.payload === 'object' && raw.payload !== null
			? (raw.payload as Record<string, unknown>)
			: {}
	return {
		seq: raw.seq,
		kind: raw.kind as RunEventKind,
		payload,
		createdAt: raw.created_at,
	}
}

function fetchEvents(runId: string, after: number) {
	return api.get('/api/v1/runs/{run_id}/events', {
		path: { run_id: runId },
		query: { after, limit: RUN_STREAM_PAGE },
	})
}

export function useRunStream(runId: string | null, active: boolean): RunStream {
	const [view, setView] = useState<View | null>(null)
	const [loadedRunId, setLoadedRunId] = useState<string | null>(null)
	const [connection, setConnection] = useState<RunStreamConnection>('idle')
	const stateRef = useRef<{ runId: string | null; state: RunStreamState }>({
		runId: null,
		state: initialRunStreamState,
	})
	const frameRef = useRef<{ cancel: () => void } | null>(null)

	useEffect(
		() => () => {
			frameRef.current?.cancel()
			frameRef.current = null
		},
		[],
	)

	useEffect(() => {
		if (runId === null) {
			stateRef.current = { runId: null, state: initialRunStreamState }
			setConnection('idle')
			return
		}
		const id = runId
		if (stateRef.current.runId !== id) {
			stateRef.current = { runId: id, state: initialRunStreamState }
		}

		let cancelled = false
		let inflight = false
		let again = false
		let subscribed = false
		let refused = false
		let graceExpired = false
		let graceTimer: ReturnType<typeof setTimeout> | null = null
		let pollTimer: ReturnType<typeof setInterval> | null = null
		let mode: RunStreamConnection = 'idle'

		function publish() {
			if (frameRef.current) return
			const flush = () => {
				frameRef.current = null
				setView({ runId: id, state: stateRef.current.state })
			}
			if (typeof requestAnimationFrame === 'function') {
				const handle = requestAnimationFrame(flush)
				frameRef.current = { cancel: () => cancelAnimationFrame(handle) }
			} else {
				const handle = setTimeout(flush, FRAME_FALLBACK_MS)
				frameRef.current = { cancel: () => clearTimeout(handle) }
			}
		}

		function apply(action: RunStreamAction) {
			if (cancelled) return
			const next = runStreamReducer(stateRef.current.state, action)
			if (next === stateRef.current.state) return
			stateRef.current = { runId: id, state: next }
			publish()
			if (next.needsFetch) void load()
		}

		async function pull() {
			for (;;) {
				apply({ type: 'fetching' })
				const after = stateRef.current.state.lastSeq
				const page = await fetchEvents(id, after)
				if (cancelled) return
				apply({ type: 'events', events: page.events.map(toEvent) })
				const next = page.next_after
				if (next == null || next <= after) return
			}
		}

		async function load() {
			if (inflight) {
				again = true
				return
			}
			inflight = true
			try {
				do {
					again = false
					try {
						await pull()
					} catch {
						again = false
					}
				} while (again && !cancelled)
			} finally {
				inflight = false
				if (!cancelled) setLoadedRunId(id)
			}
		}

		function setMode(next: RunStreamConnection) {
			if (next === mode) return
			mode = next
			setConnection(next)
			if (next === 'polling') {
				if (pollTimer === null) {
					pollTimer = setInterval(() => void load(), RUN_STREAM_POLL_MS)
				}
			} else if (pollTimer !== null) {
				clearInterval(pollTimer)
				pollTimer = null
			}
		}

		function evaluate() {
			const state = realtimeStore.state.state
			if (state !== 'connected' || refused || graceExpired) {
				setMode('polling')
			} else if (subscribed) {
				setMode('live')
			} else {
				setMode('idle')
			}
		}

		function armGrace() {
			if (graceTimer !== null) clearTimeout(graceTimer)
			graceExpired = false
			graceTimer = setTimeout(() => {
				graceTimer = null
				graceExpired = true
				evaluate()
			}, RUN_STREAM_SUBSCRIBE_TIMEOUT_MS)
		}

		if (!active) {
			setMode('idle')
			void load().then(() => apply({ type: 'clearLive' }))
			return () => {
				cancelled = true
			}
		}

		void load()

		let wasConnected = realtimeStore.state.state === 'connected'
		armGrace()
		evaluate()

		const watch = realtimeStore.subscribe(() => {
			const nowConnected = realtimeStore.state.state === 'connected'
			if (nowConnected !== wasConnected) {
				wasConnected = nowConnected
				subscribed = false
				if (nowConnected) armGrace()
			}
			evaluate()
		})

		const leave = subscribeRun(id, {
			onSubscribed() {
				subscribed = true
				refused = false
				graceExpired = false
				if (graceTimer !== null) {
					clearTimeout(graceTimer)
					graceTimer = null
				}
				evaluate()
				void load()
			},
			onRefused() {
				refused = true
				evaluate()
			},
			onDelta(after, text) {
				apply({ type: 'delta', after, text })
			},
			onEvents(last) {
				apply({ type: 'stored', last })
			},
			onResync() {
				apply({ type: 'resync' })
			},
		})

		return () => {
			watch.unsubscribe()
			leave()
			if (graceTimer !== null) clearTimeout(graceTimer)
			if (pollTimer !== null) clearInterval(pollTimer)
			cancelled = true
		}
	}, [runId, active])

	if (runId === null) {
		return { events: [], live: '', connection: 'idle', loading: false }
	}
	const state = view?.runId === runId ? view.state : initialRunStreamState
	return {
		events: state.events,
		live: state.live,
		connection: active ? connection : 'idle',
		loading: loadedRunId !== runId,
	}
}

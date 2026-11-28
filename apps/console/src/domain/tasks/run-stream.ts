export type RunEventKind =
	| 'assistant_text'
	| 'tool_use'
	| 'tool_result'
	| 'system'

export interface RunStreamEvent {
	seq: number
	kind: RunEventKind
	payload: Record<string, unknown>
	createdAt: string
}

export type RunStreamConnection = 'live' | 'polling' | 'idle'

export interface RunStream {
	events: RunStreamEvent[]
	live: string
	connection: RunStreamConnection
	loading: boolean
}

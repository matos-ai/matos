import type { Schemas } from '#/api/api.client'
import { isActiveRun } from '#/domain/tasks/run-agent'
import type { RunStreamEvent } from '#/domain/tasks/run-stream'

export type ToolStatus = 'pending' | 'ok' | 'failed'

export type TurnBlock =
	| { type: 'text'; key: number; text: string; truncated: boolean }
	| {
			type: 'tool'
			key: number
			tool: string
			summary: string
			input: string
			output: string | null
			status: ToolStatus
	  }
	| { type: 'system'; key: number; text: string }

const SUMMARY_CHARS = 100
const FOLD_LINES = 12
const FOLD_CHARS = 1200
const BOTTOM_PX = 48
const RUN_END_SLACK_MS = 5000

function asText(value: unknown): string {
	return typeof value === 'string' ? value : ''
}

export function formatValue(value: unknown): string {
	if (value === undefined || value === null) return ''
	if (typeof value === 'string') return value
	return JSON.stringify(value, null, 2) ?? ''
}

const HEADLINE_KEYS = [
	'file_path',
	'path',
	'command',
	'pattern',
	'url',
	'query',
	'description',
	'prompt',
]

function headline(raw: string): string {
	try {
		const parsed: unknown = JSON.parse(raw)
		if (
			typeof parsed !== 'object' ||
			parsed === null ||
			Array.isArray(parsed)
		) {
			return raw
		}
		const fields = parsed as Record<string, unknown>
		for (const key of HEADLINE_KEYS) {
			const value = fields[key]
			if (typeof value === 'string' && value.length > 0) return value
		}
		return raw
	} catch {
		return raw
	}
}

export function summarizeInput(
	input: unknown,
	max: number = SUMMARY_CHARS,
): string {
	if (input === undefined || input === null) return ''
	const raw = typeof input === 'string' ? input : (JSON.stringify(input) ?? '')
	const line = headline(raw).replace(/\s+/g, ' ').trim()
	return line.length > max ? line.slice(0, max) : line
}

export function foldValue(
	text: string,
	limits: { maxLines?: number; maxChars?: number } = {},
): { shown: string; folded: boolean } {
	const maxLines = limits.maxLines ?? FOLD_LINES
	const maxChars = limits.maxChars ?? FOLD_CHARS
	const lines = text.split('\n')
	if (lines.length <= maxLines && text.length <= maxChars) {
		return { shown: text, folded: false }
	}
	return {
		shown: lines.slice(0, maxLines).join('\n').slice(0, maxChars),
		folded: true,
	}
}

export function turnBlocks(events: ReadonlyArray<RunStreamEvent>): TurnBlock[] {
	const ordered = [...events].sort((a, b) => a.seq - b.seq)
	const results = new Map<string, RunStreamEvent>()
	const called = new Set<string>()
	for (const item of ordered) {
		if (item.kind === 'tool_result') {
			results.set(asText(item.payload.id), item)
		}
		if (item.kind === 'tool_use') called.add(asText(item.payload.id))
	}
	const blocks: TurnBlock[] = []
	for (const item of ordered) {
		const { payload } = item
		if (item.kind === 'assistant_text') {
			blocks.push({
				type: 'text',
				key: item.seq,
				text: asText(payload.text),
				truncated: payload.truncated === true,
			})
		} else if (item.kind === 'system') {
			blocks.push({ type: 'system', key: item.seq, text: asText(payload.text) })
		} else if (item.kind === 'tool_use') {
			const result = results.get(asText(payload.id))
			blocks.push({
				type: 'tool',
				key: item.seq,
				tool: asText(payload.tool),
				summary: summarizeInput(payload.input),
				input: formatValue(payload.input),
				output: result ? formatValue(result.payload.output) : null,
				status: result
					? result.payload.succeeded === true
						? 'ok'
						: 'failed'
					: 'pending',
			})
		} else if (item.kind === 'tool_result' && !called.has(asText(payload.id))) {
			blocks.push({
				type: 'tool',
				key: item.seq,
				tool: 'Résultat',
				summary: '',
				input: '',
				output: formatValue(payload.output),
				status: payload.succeeded === true ? 'ok' : 'failed',
			})
		}
	}
	return blocks
}

export function isNearBottom(
	metrics: { scrollTop: number; clientHeight: number; scrollHeight: number },
	threshold: number = BOTTOM_PX,
): boolean {
	return (
		metrics.scrollHeight - metrics.scrollTop - metrics.clientHeight <= threshold
	)
}

export type ThreadItem<C> =
	| { type: 'comment'; comment: C }
	| { type: 'run'; run: Schemas.RunResponse }

interface Timed {
	created_at: string
	author_actor?: string | null
}

export function isAgentComment(comment: Timed): boolean {
	return comment.author_actor?.startsWith('agent:') ?? false
}

export function placeRuns<C extends Timed>(
	comments: ReadonlyArray<C>,
	runs: ReadonlyArray<Schemas.RunResponse>,
): ThreadItem<C>[] {
	const after = new Map<number, Schemas.RunResponse[]>()
	const finished = runs
		.filter((run) => !isActiveRun(run.status))
		.sort((a, b) => Date.parse(a.created_at) - Date.parse(b.created_at))
	for (const run of finished) {
		const start = Date.parse(run.created_at)
		const end = Date.parse(run.finished_at ?? run.created_at) + RUN_END_SLACK_MS
		let anchor = -1
		let fallback = -1
		comments.forEach((comment, index) => {
			const at = Date.parse(comment.created_at)
			if (at <= end) fallback = index
			if (isAgentComment(comment) && at >= start && at <= end) anchor = index
		})
		const index = anchor >= 0 ? anchor : fallback
		after.set(index, [...(after.get(index) ?? []), run])
	}
	const items: ThreadItem<C>[] = (after.get(-1) ?? []).map((run) => ({
		type: 'run',
		run,
	}))
	comments.forEach((comment, index) => {
		items.push({ type: 'comment', comment })
		for (const run of after.get(index) ?? []) items.push({ type: 'run', run })
	})
	return items
}

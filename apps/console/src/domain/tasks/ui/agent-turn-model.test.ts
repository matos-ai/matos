import { describe, expect, it } from 'vitest'
import type { RunStreamEvent } from '#/domain/tasks/run-stream'
import {
	foldValue,
	formatValue,
	isNearBottom,
	placeRuns,
	summarizeInput,
	turnBlocks,
} from '#/domain/tasks/ui/agent-turn-model'

const event = (
	seq: number,
	kind: RunStreamEvent['kind'],
	payload: Record<string, unknown>,
): RunStreamEvent => ({ seq, kind, payload, createdAt: '2026-11-01T00:00:00Z' })

describe('turnBlocks', () => {
	it('keeps the stream order and pairs a tool call with its result', () => {
		const blocks = turnBlocks([
			event(3, 'tool_result', { id: 't1', succeeded: true, output: 'ok' }),
			event(1, 'assistant_text', { text: 'Je regarde', truncated: false }),
			event(2, 'tool_use', { id: 't1', tool: 'lire', input: { chemin: 'a' } }),
			event(4, 'system', { text: 'Reprise' }),
		])
		expect(blocks.map((b) => b.type)).toEqual(['text', 'tool', 'system'])
		expect(blocks[1]).toMatchObject({
			type: 'tool',
			tool: 'lire',
			status: 'ok',
			output: 'ok',
		})
	})

	it('marks a call without result as pending and a failed result', () => {
		const blocks = turnBlocks([
			event(1, 'tool_use', { id: 'a', tool: 'x', input: {} }),
			event(2, 'tool_use', { id: 'b', tool: 'y', input: {} }),
			event(3, 'tool_result', { id: 'b', succeeded: false, output: 'boom' }),
		])
		expect(blocks.map((b) => b.type === 'tool' && b.status)).toEqual([
			'pending',
			'failed',
		])
	})

	it('keeps a result with no call as its own row', () => {
		const blocks = turnBlocks([
			event(1, 'tool_result', { id: 'z', succeeded: true, output: 'o' }),
		])
		expect(blocks).toHaveLength(1)
		expect(blocks[0]).toMatchObject({ type: 'tool', status: 'ok' })
	})

	it('carries the truncation flag of a text', () => {
		const [block] = turnBlocks([
			event(1, 'assistant_text', { text: 'a', truncated: true }),
		])
		expect(block).toMatchObject({ type: 'text', truncated: true })
	})
})

describe('values', () => {
	it('summarises an input on one line', () => {
		expect(summarizeInput({ a: 1 })).toBe('{"a":1}')
		expect(summarizeInput('x\ny')).toBe('x y')
		expect(summarizeInput({ t: 'x'.repeat(200) }, 20)).toHaveLength(20)
		expect(summarizeInput(undefined)).toBe('')
	})

	it('formats strings as is and others as JSON', () => {
		expect(formatValue('abc')).toBe('abc')
		expect(formatValue({ a: 1 })).toBe('{\n  "a": 1\n}')
		expect(formatValue(null)).toBe('')
	})

	it('folds a long value by lines or characters', () => {
		const lines = Array.from({ length: 30 }, (_, i) => `l${i}`).join('\n')
		const folded = foldValue(lines, { maxLines: 5, maxChars: 1000 })
		expect(folded.folded).toBe(true)
		expect(folded.shown.split('\n')).toHaveLength(5)
		expect(foldValue('court').folded).toBe(false)
		const chars = foldValue('x'.repeat(50), { maxLines: 10, maxChars: 20 })
		expect(chars.shown).toHaveLength(20)
		expect(chars.folded).toBe(true)
	})
})

describe('isNearBottom', () => {
	it('is true within the threshold and false above it', () => {
		expect(
			isNearBottom({ scrollTop: 900, clientHeight: 100, scrollHeight: 1000 }),
		).toBe(true)
		expect(
			isNearBottom({ scrollTop: 860, clientHeight: 100, scrollHeight: 1000 }),
		).toBe(true)
		expect(
			isNearBottom({ scrollTop: 500, clientHeight: 100, scrollHeight: 1000 }),
		).toBe(false)
		expect(
			isNearBottom({ scrollTop: 0, clientHeight: 0, scrollHeight: 0 }),
		).toBe(true)
	})
})

describe('placeRuns', () => {
	const comment = (id: string, at: string, actor: string | null) => ({
		id,
		author_subject: 'u',
		author_actor: actor,
		body: id,
		created_at: at,
	})
	const run = (id: string, created: string, finished: string | null) => ({
		id,
		status: 'succeeded' as const,
		created_at: created,
		finished_at: finished,
		initiated_by: 'u',
		pipeline: [],
	})

	it('puts a finished run under the last agent comment of its window', () => {
		const items = placeRuns(
			[
				comment('h1', '2026-11-01T10:00:00Z', null),
				comment('a1', '2026-11-01T10:01:00Z', 'agent:lead'),
				comment('h2', '2026-11-01T10:02:00Z', null),
			],
			[run('r1', '2026-11-01T10:00:30Z', '2026-11-01T10:01:01Z')],
		)
		expect(
			items.map((i) => (i.type === 'run' ? `run:${i.run.id}` : i.comment.id)),
		).toEqual(['h1', 'a1', 'run:r1', 'h2'])
	})

	it('falls back to the last comment before the run ended', () => {
		const items = placeRuns(
			[
				comment('h1', '2026-11-01T10:00:00Z', null),
				comment('h2', '2026-11-01T10:05:00Z', null),
			],
			[run('r1', '2026-11-01T10:00:30Z', '2026-11-01T10:01:00Z')],
		)
		expect(items.map((i) => (i.type === 'run' ? 'run' : i.comment.id))).toEqual(
			['h1', 'run', 'h2'],
		)
	})

	it('ignores active runs', () => {
		const items = placeRuns(
			[],
			[{ ...run('r1', '2026-11-01T10:00:00Z', null), status: 'running' }],
		)
		expect(items).toEqual([])
	})
})

describe('summarizeInput headline', () => {
	it('shows the path of a file tool instead of its raw JSON', () => {
		expect(summarizeInput('{"file_path":"/workspace/repo/README.md"}')).toBe(
			'/workspace/repo/README.md',
		)
	})

	it('prefers the command, then falls back to the compact JSON', () => {
		expect(summarizeInput('{"command":"ls -la","timeout":5}')).toBe('ls -la')
		expect(summarizeInput('{"limit":5}')).toBe('{"limit":5}')
		expect(summarizeInput('not json')).toBe('not json')
	})
})

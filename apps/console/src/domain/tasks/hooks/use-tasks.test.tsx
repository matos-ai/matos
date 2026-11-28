import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, renderHook, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { tanstackApi } from '#/api'
import type { Schemas } from '#/api/api.client'
import { useMoveCard, useUpdateCard } from '#/domain/tasks/hooks/use-tasks'
import { card } from '#/domain/tasks/test-fixtures'
import { clearAuth } from '#/store/auth.store'
import { mockApi } from '#/test-utils/api-mock'

const ID = 'c0000000-0000-4000-8000-000000000001'
const OTHER = 'c0000000-0000-4000-8000-000000000002'
const AGENT = 'a0000000-0000-4000-8000-000000000001'

const listKey = tanstackApi.get('/api/v1/cards', { query: {} }).queryOptions
	.queryKey
const detailKey = tanstackApi.get('/api/v1/cards/{card_id}', {
	path: { card_id: ID },
}).queryOptions.queryKey

function setup(routes: Parameters<typeof mockApi>[0]) {
	const fetchMock = mockApi(routes)
	const client = new QueryClient({
		defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
	})
	client.setQueryData(listKey, {
		items: [card(ID), card(OTHER)],
	} satisfies Schemas.CardListResponse)
	client.setQueryData(detailKey, {
		card: card(ID),
		comments: [],
		runs: [],
	} satisfies Schemas.CardDetailResponse)
	const wrapper = ({ children }: { children: ReactNode }) => (
		<QueryClientProvider client={client}>{children}</QueryClientProvider>
	)
	const invalidate = vi.spyOn(client, 'invalidateQueries')
	return { client, wrapper, fetchMock, invalidate }
}

function gate(fetchMock: ReturnType<typeof mockApi>) {
	const original = fetchMock.getMockImplementation()
	let release: () => void = () => {}
	const opened = new Promise<void>((resolve) => {
		release = resolve
	})
	fetchMock.mockImplementation(async (...args) => {
		await opened
		return original?.(...args) as Promise<Response>
	})
	return release
}

const listOf = (client: QueryClient) =>
	(client.getQueryData(listKey) as Schemas.CardListResponse).items
const detailOf = (client: QueryClient) =>
	client.getQueryData(detailKey) as Schemas.CardDetailResponse

const CONFLICT = {
	status: 409,
	body: { code: 'version_conflict', message: 'stale', status: 409 },
}

afterEach(() => {
	vi.unstubAllGlobals()
	clearAuth()
})

describe('useUpdateCard', () => {
	const vars = {
		path: { card_id: ID },
		body: { assignee_agent_id: AGENT, priority: 'high', expected_version: 3 },
	} as const

	it('shows the new value at once in list and detail, then settles', async () => {
		const { client, wrapper, fetchMock, invalidate } = setup({
			[`PATCH /api/v1/cards/${ID}`]: card(ID, {
				assignee_agent_id: AGENT,
				priority: 'high',
			}),
		})
		const release = gate(fetchMock)
		const { result } = renderHook(() => useUpdateCard(), { wrapper })
		act(() => result.current.mutate(vars))
		await waitFor(() =>
			expect(detailOf(client).card.assignee_agent_id).toBe(AGENT),
		)
		expect(detailOf(client).card.priority).toBe('high')
		expect(listOf(client)[0]?.assignee_agent_id).toBe(AGENT)
		expect(listOf(client)[1]?.assignee_agent_id).toBeUndefined()
		release()
		await waitFor(() => expect(result.current.isSuccess).toBe(true))
		await waitFor(() => expect(invalidate).toHaveBeenCalled())
	})

	it('rolls back and invalidates on error', async () => {
		const { client, wrapper, fetchMock, invalidate } = setup({
			[`PATCH /api/v1/cards/${ID}`]: { status: 500, body: { code: 'x' } },
		})
		const release = gate(fetchMock)
		const { result } = renderHook(() => useUpdateCard(), { wrapper })
		act(() => result.current.mutate(vars))
		await waitFor(() => expect(detailOf(client).card.priority).toBe('high'))
		release()
		await waitFor(() => expect(result.current.isError).toBe(true))
		expect(detailOf(client).card.priority).toBe('normal')
		expect(listOf(client)[0]?.priority).toBe('normal')
		await waitFor(() => expect(invalidate).toHaveBeenCalled())
	})

	it('rolls back, invalidates and surfaces a 409', async () => {
		const { client, wrapper, invalidate } = setup({
			[`PATCH /api/v1/cards/${ID}`]: CONFLICT,
		})
		const { result } = renderHook(() => useUpdateCard(), { wrapper })
		act(() => result.current.mutate(vars))
		await waitFor(() => expect(result.current.isError).toBe(true))
		expect((result.current.error as { status?: number }).status).toBe(409)
		expect(detailOf(client).card.priority).toBe('normal')
		await waitFor(() => expect(invalidate).toHaveBeenCalled())
	})

	it('clears a nullable field when null is sent', async () => {
		const { client, wrapper, fetchMock } = setup({
			[`PATCH /api/v1/cards/${ID}`]: card(ID),
		})
		client.setQueryData(detailKey, {
			...detailOf(client),
			card: card(ID, { assignee_agent_id: AGENT }),
		})
		const release = gate(fetchMock)
		const { result } = renderHook(() => useUpdateCard(), { wrapper })
		act(() =>
			result.current.mutate({
				path: { card_id: ID },
				body: { assignee_agent_id: null, expected_version: 3 },
			}),
		)
		await waitFor(() =>
			expect(detailOf(client).card.assignee_agent_id).toBeNull(),
		)
		release()
	})
})

describe('useMoveCard', () => {
	const vars = {
		path: { card_id: ID },
		body: { to: 'in_progress', expected_version: 3 },
	} as const

	it('moves the card between columns in list and detail at once', async () => {
		const { client, wrapper, fetchMock } = setup({
			[`POST /api/v1/cards/${ID}/move`]: card(ID, { column: 'in_progress' }),
		})
		const release = gate(fetchMock)
		const { result } = renderHook(() => useMoveCard(), { wrapper })
		act(() => result.current.mutate(vars))
		await waitFor(() =>
			expect(detailOf(client).card.column).toBe('in_progress'),
		)
		expect(listOf(client)[0]?.column).toBe('in_progress')
		expect(listOf(client)[1]?.column).toBe('todo')
		release()
		await waitFor(() => expect(result.current.isSuccess).toBe(true))
	})

	it('rolls back and invalidates on a 409', async () => {
		const { client, wrapper, invalidate } = setup({
			[`POST /api/v1/cards/${ID}/move`]: CONFLICT,
		})
		const { result } = renderHook(() => useMoveCard(), { wrapper })
		act(() => result.current.mutate(vars))
		await waitFor(() => expect(result.current.isError).toBe(true))
		expect(detailOf(client).card.column).toBe('todo')
		expect(listOf(client)[0]?.column).toBe('todo')
		await waitFor(() => expect(invalidate).toHaveBeenCalled())
	})
})

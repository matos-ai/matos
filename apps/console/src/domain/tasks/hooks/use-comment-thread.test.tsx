import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, renderHook, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { tanstackApi } from '#/api'
import type { Schemas } from '#/api/api.client'
import {
	discardFailedComment,
	mergeLocalComments,
	type PendingComment,
	useCommentCard,
	useRetryComment,
} from '#/domain/tasks/hooks/use-comment-thread'
import { useCard } from '#/domain/tasks/hooks/use-tasks'
import { card, jsonBody } from '#/domain/tasks/test-fixtures'
import { clearAuth } from '#/store/auth.store'
import { mockApi } from '#/test-utils/api-mock'

const ID = 'c0000000-0000-4000-8000-000000000001'
const PATH = `/api/v1/cards/${ID}`
const COMMENTS = `POST ${PATH}/comments`

const detailKey = tanstackApi.get('/api/v1/cards/{card_id}', {
	path: { card_id: ID },
}).queryOptions.queryKey
const meKey = tanstackApi.get('/api/v1/me').queryOptions.queryKey

const server = (
	id: string,
	body: string,
	createdAt: string,
): Schemas.CommentResponse => ({
	id,
	author_subject: 'sub-1',
	body,
	created_at: createdAt,
})

function setup(
	routes: Parameters<typeof mockApi>[0],
	comments: PendingComment[] = [],
) {
	const fetchMock = mockApi(routes)
	const client = new QueryClient({
		defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
	})
	client.setQueryData(meKey, { subject: 'sub-1' } as Schemas.MeResponse)
	client.setQueryData(detailKey, {
		card: card(ID),
		comments,
		runs: [],
	} satisfies Schemas.CardDetailResponse)
	const wrapper = ({ children }: { children: ReactNode }) => (
		<QueryClientProvider client={client}>{children}</QueryClientProvider>
	)
	return { client, wrapper, fetchMock }
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

const commentsOf = (client: QueryClient) =>
	(client.getQueryData(detailKey) as { comments: PendingComment[] }).comments

const FAIL = { status: 500, body: { code: 'boom', message: 'x', status: 500 } }

const post = (body: string, interrupt?: boolean) => ({
	path: { card_id: ID },
	body: { body, ...(interrupt ? { interrupt } : {}) },
})

afterEach(() => {
	vi.unstubAllGlobals()
	clearAuth()
})

describe('useCommentCard', () => {
	it('shows a pending comment at once, then the server comment replaces it', async () => {
		const posted = server('real-1', 'salut', '2030-01-01T00:00:00Z')
		const { client, wrapper, fetchMock } = setup({ [COMMENTS]: posted })
		const release = gate(fetchMock)
		const { result } = renderHook(() => useCommentCard(), { wrapper })
		act(() => result.current.mutate(post('salut')))
		await waitFor(() => expect(commentsOf(client)).toHaveLength(1))
		expect(commentsOf(client)[0]).toMatchObject({
			body: 'salut',
			pending: true,
			author_subject: 'sub-1',
		})
		expect(commentsOf(client)[0]?.id).toMatch(/^pending-/)
		release()
		await waitFor(() => expect(result.current.isSuccess).toBe(true))
		expect(commentsOf(client)).toEqual([posted])
	})

	it('keeps the comment with failed: true on error', async () => {
		const { client, wrapper } = setup({ [COMMENTS]: FAIL })
		const { result } = renderHook(() => useCommentCard(), { wrapper })
		act(() => result.current.mutate(post('perdu', true)))
		await waitFor(() => expect(result.current.isError).toBe(true))
		expect(commentsOf(client)).toHaveLength(1)
		expect(commentsOf(client)[0]).toMatchObject({
			body: 'perdu',
			failed: true,
			pending: false,
			interrupt: true,
		})
	})

	it('keeps a failed comment through a refetch of the card', async () => {
		const { client, wrapper } = setup({
			[COMMENTS]: FAIL,
			[`GET ${PATH}`]: {
				card: card(ID),
				comments: [server('s1', 'autre', '2030-01-01T00:00:00Z')],
				runs: [],
			},
		})
		const card$ = renderHook(() => useCard(ID), { wrapper })
		const { result } = renderHook(() => useCommentCard(), { wrapper })
		await waitFor(() =>
			expect(card$.result.current.data?.comments).toHaveLength(1),
		)
		act(() => result.current.mutate(post('perdu')))
		await waitFor(() => expect(result.current.isError).toBe(true))
		await act(() => client.invalidateQueries())
		await waitFor(() => expect(commentsOf(client)).toHaveLength(2))
		expect(commentsOf(client).map((c) => c.id)[0]).toBe('s1')
		expect(commentsOf(client)[1]).toMatchObject({
			body: 'perdu',
			failed: true,
		})
	})
})

describe('mergeLocalComments', () => {
	const local = (extra: Partial<PendingComment>): PendingComment => ({
		id: 'pending-1',
		author_subject: 'sub-1',
		body: 'hello',
		created_at: '2030-01-01T00:00:00.000Z',
		...extra,
	})
	const detail = (comments: PendingComment[]) => ({ comments })

	it('drops a pending comment once the server has the same one, created later', () => {
		const next = detail([server('s1', 'hello', '2030-01-01T00:00:01.000Z')])
		expect(
			mergeLocalComments(detail([local({ pending: true })]), next),
		).toEqual(next)
	})

	it('keeps it when the server copy is older or from another author', () => {
		const older = server('s1', 'hello', '2029-12-31T00:00:00.000Z')
		const other = {
			...server('s2', 'hello', '2030-01-01T00:00:01.000Z'),
			author_subject: 'sub-2',
		}
		const merged = mergeLocalComments(
			detail([local({ failed: true })]),
			detail([older, other]),
		) as { comments: PendingComment[] }
		expect(merged.comments.map((c) => c.id)).toEqual(['s1', 's2', 'pending-1'])
	})
})

describe('useRetryComment', () => {
	const failed: PendingComment = {
		id: 'pending-9',
		author_subject: 'sub-1',
		body: 'perdu',
		created_at: '2030-01-01T00:00:00.000Z',
		failed: true,
		interrupt: true,
	}

	it('resends the body once with the same interrupt flag and replaces the entry', async () => {
		const posted = server('real-9', 'perdu', '2030-01-01T00:00:05Z')
		const { client, wrapper, fetchMock } = setup({ [COMMENTS]: posted }, [
			failed,
		])
		const release = gate(fetchMock)
		const { result } = renderHook(() => useRetryComment(), { wrapper })
		act(() => result.current.mutate({ cardId: ID, tempId: 'pending-9' }))
		await waitFor(() =>
			expect(commentsOf(client)[0]).toMatchObject({
				pending: true,
				failed: false,
			}),
		)
		release()
		await waitFor(() => expect(result.current.isSuccess).toBe(true))
		expect(commentsOf(client)).toEqual([posted])
		const posts = fetchMock.mock.calls.filter(
			([, init]) => init?.method === 'POST',
		)
		expect(posts).toHaveLength(1)
		expect(jsonBody(fetchMock, 'POST', `${PATH}/comments`)).toEqual({
			body: 'perdu',
			interrupt: true,
		})
	})

	it('goes back to failed when the retry fails too', async () => {
		const { client, wrapper } = setup({ [COMMENTS]: FAIL }, [failed])
		const { result } = renderHook(() => useRetryComment(), { wrapper })
		act(() => result.current.mutate({ cardId: ID, tempId: 'pending-9' }))
		await waitFor(() => expect(result.current.isError).toBe(true))
		expect(commentsOf(client)).toHaveLength(1)
		expect(commentsOf(client)[0]).toMatchObject({
			failed: true,
			pending: false,
		})
	})

	it('refuses an entry that is not failed and sends nothing', async () => {
		const { wrapper, fetchMock } = setup({ [COMMENTS]: FAIL }, [
			{ ...failed, failed: false, pending: true },
		])
		const { result } = renderHook(() => useRetryComment(), { wrapper })
		act(() => result.current.mutate({ cardId: ID, tempId: 'pending-9' }))
		await waitFor(() => expect(result.current.isError).toBe(true))
		expect(fetchMock).not.toHaveBeenCalled()
	})
})

describe('discardFailedComment', () => {
	it('removes the failed entry only', () => {
		const { client } = setup({}, [
			{
				id: 'pending-1',
				author_subject: 'sub-1',
				body: 'a',
				created_at: '2030-01-01T00:00:00Z',
				failed: true,
			},
			{
				id: 'pending-2',
				author_subject: 'sub-1',
				body: 'b',
				created_at: '2030-01-01T00:00:00Z',
				pending: true,
			},
		])
		discardFailedComment(client, ID, 'pending-2')
		expect(commentsOf(client)).toHaveLength(2)
		discardFailedComment(client, ID, 'pending-1')
		expect(commentsOf(client).map((c) => c.id)).toEqual(['pending-2'])
	})
})

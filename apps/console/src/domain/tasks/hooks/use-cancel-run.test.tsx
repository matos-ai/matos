import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { act, renderHook, waitFor } from '@testing-library/react'
import type { ReactNode } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import {
	cancellingStore,
	useCancelRun,
	useIsCancelling,
} from '#/domain/tasks/hooks/use-cancel-run'
import { clearAuth } from '#/store/auth.store'
import { mockApi } from '#/test-utils/api-mock'

const RUN = 'r0000000-0000-4000-8000-000000000001'
const CARD = 'c0000000-0000-4000-8000-000000000001'
const ROUTE = `POST /api/v1/runs/${RUN}/cancel`

function setup(routes: Parameters<typeof mockApi>[0]) {
	const fetchMock = mockApi(routes)
	const client = new QueryClient({
		defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
	})
	const wrapper = ({ children }: { children: ReactNode }) => (
		<QueryClientProvider client={client}>{children}</QueryClientProvider>
	)
	const invalidate = vi.spyOn(client, 'invalidateQueries')
	return { wrapper, fetchMock, invalidate }
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

afterEach(() => {
	vi.unstubAllGlobals()
	clearAuth()
	cancellingStore.setState(() => ({ runIds: [] }))
})

describe('useCancelRun', () => {
	it('marks the run at once, clears on success and invalidates runs and cards', async () => {
		const { wrapper, fetchMock, invalidate } = setup({
			[ROUTE]: { status: 'cancelled', already_cancelled: false },
		})
		const release = gate(fetchMock)
		const { result } = renderHook(
			() => ({ cancel: useCancelRun(), busy: useIsCancelling(RUN) }),
			{ wrapper },
		)
		const other = renderHook(() => useIsCancelling('other'), { wrapper })
		expect(result.current.busy).toBe(false)
		act(() => result.current.cancel.mutate({ runId: RUN, cardId: CARD }))
		await waitFor(() => expect(result.current.busy).toBe(true))
		expect(other.result.current).toBe(false)
		release()
		await waitFor(() => expect(result.current.cancel.isSuccess).toBe(true))
		expect(result.current.busy).toBe(false)
		const predicates = invalidate.mock.calls.map(
			([filters]) =>
				(filters as { predicate: (q: unknown) => boolean }).predicate,
		)
		const probe = (id: string) => ({ queryKey: [{ _id: id }] })
		expect(predicates.some((p) => p(probe('/api/v1/runs/{run_id}')))).toBe(true)
		expect(predicates.some((p) => p(probe('/api/v1/cards/{card_id}')))).toBe(
			true,
		)
	})

	it('clears the marker on error and carries the error', async () => {
		const { wrapper, fetchMock, invalidate } = setup({
			[ROUTE]: {
				status: 409,
				body: { code: 'run_already_finished', message: 'x', status: 409 },
			},
		})
		const release = gate(fetchMock)
		const { result } = renderHook(
			() => ({ cancel: useCancelRun(), busy: useIsCancelling(RUN) }),
			{ wrapper },
		)
		act(() => result.current.cancel.mutate({ runId: RUN, cardId: CARD }))
		await waitFor(() => expect(result.current.busy).toBe(true))
		release()
		await waitFor(() => expect(result.current.cancel.isError).toBe(true))
		expect(result.current.busy).toBe(false)
		expect((result.current.cancel.error as { status?: number }).status).toBe(
			409,
		)
		expect(invalidate).not.toHaveBeenCalled()
	})

	it('is never cancelling for a null run', () => {
		const { wrapper } = setup({})
		const { result } = renderHook(() => useIsCancelling(null), { wrapper })
		expect(result.current).toBe(false)
	})
})

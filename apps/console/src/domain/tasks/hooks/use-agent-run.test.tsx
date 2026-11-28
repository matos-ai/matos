import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import { renderHook } from '@testing-library/react'
import type { ReactNode } from 'react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { useRunPolling } from '#/domain/tasks/hooks/use-agent-run'
import { RUN_POLL_MS } from '#/domain/tasks/run-agent'
import { clearAuth } from '#/store/auth.store'
import { mockApi } from '#/test-utils/api-mock'

afterEach(() => {
	vi.unstubAllGlobals()
	clearAuth()
})

function interval(poll: boolean, status: string | undefined) {
	mockApi({})
	const client = new QueryClient()
	const wrapper = ({ children }: { children: ReactNode }) => (
		<QueryClientProvider client={client}>{children}</QueryClientProvider>
	)
	renderHook(() => useRunPolling('r1', poll), { wrapper })
	const observer = client.getQueryCache().getAll()[0]?.observers[0]
	const option = observer?.options.refetchInterval as (
		query: unknown,
	) => unknown
	return option({ state: { data: status ? { status } : undefined } })
}

describe('useRunPolling', () => {
	it('polls while the run is active and polling is on', () => {
		expect(interval(true, 'running')).toBe(RUN_POLL_MS)
		expect(interval(true, undefined)).toBe(RUN_POLL_MS)
	})

	it('stops once the run is over', () => {
		expect(interval(true, 'succeeded')).toBe(false)
	})

	it('never polls when realtime is connected', () => {
		expect(interval(false, 'running')).toBe(false)
	})
})

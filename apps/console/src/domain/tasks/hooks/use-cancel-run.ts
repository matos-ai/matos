import {
	type Query,
	type QueryClient,
	useMutation,
	useQueryClient,
} from '@tanstack/react-query'
import { Store } from '@tanstack/store'
import { useSyncExternalStore } from 'react'
import { tanstackApi } from '#/api'

export const cancellingStore = new Store<{ runIds: readonly string[] }>({
	runIds: [],
})

const RUNS_ID = '/api/v1/runs'
const CARDS_ID = '/api/v1/cards'

function markCancelling(runId: string): void {
	cancellingStore.setState((state) =>
		state.runIds.includes(runId) ? state : { runIds: [...state.runIds, runId] },
	)
}

function clearCancelling(runId: string): void {
	cancellingStore.setState((state) =>
		state.runIds.includes(runId)
			? { runIds: state.runIds.filter((id) => id !== runId) }
			: state,
	)
}

function idStartsWith(query: Query, prefix: string): boolean {
	const head: unknown = query.queryKey[0]
	const id =
		typeof head === 'object' && head !== null
			? (head as { _id?: string })._id
			: undefined
	return id?.startsWith(prefix) ?? false
}

function invalidateRunAndCards(client: QueryClient) {
	return Promise.all([
		client.invalidateQueries({
			predicate: (query) => idStartsWith(query, RUNS_ID),
		}),
		client.invalidateQueries({
			predicate: (query) => idStartsWith(query, CARDS_ID),
		}),
	])
}

export interface CancelRunVars {
	runId: string
	cardId: string
}

export function useCancelRun() {
	const client = useQueryClient()
	const cancel = tanstackApi.mutation('post', '/api/v1/runs/{run_id}/cancel')
	return useMutation({
		mutationKey: cancel.mutationKey,
		mutationFn: ({ runId }: CancelRunVars) =>
			cancel.mutationOptions.mutationFn({ path: { run_id: runId } }),
		onMutate: ({ runId }) => markCancelling(runId),
		onSuccess: () => {
			void invalidateRunAndCards(client)
		},
		onSettled: (_data, _error, { runId }) => clearCancelling(runId),
	})
}

export function useIsCancelling(runId: string | null): boolean {
	return useSyncExternalStore(
		(onChange) => cancellingStore.subscribe(onChange).unsubscribe,
		() => runId !== null && cancellingStore.state.runIds.includes(runId),
		() => false,
	)
}

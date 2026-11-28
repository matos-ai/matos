import type { QueryClient, QueryFilters, QueryKey } from '@tanstack/react-query'

export type OptimisticSnapshot = ReadonlyArray<readonly [QueryKey, unknown]>

export interface OptimisticContext {
	snapshot: OptimisticSnapshot
}

export interface OptimisticOptions<TVars> {
	filters: (vars: TVars) => QueryFilters
	update: (data: unknown, vars: TVars, queryKey: QueryKey) => unknown
	invalidate: (client: QueryClient) => Promise<unknown>
}

export function optimistic<TVars>(
	client: QueryClient,
	options: OptimisticOptions<TVars>,
) {
	return {
		async onMutate(vars: TVars): Promise<OptimisticContext> {
			const filters = options.filters(vars)
			await client.cancelQueries(filters)
			const snapshot = client
				.getQueriesData(filters)
				.filter(([, data]) => data !== undefined)
			for (const [key] of snapshot) {
				client.setQueryData(key, (data: unknown) =>
					data === undefined ? data : options.update(data, vars, key),
				)
			}
			return { snapshot }
		},
		onError(
			_error: unknown,
			_vars: TVars,
			context: OptimisticContext | undefined,
		): void {
			for (const [key, data] of context?.snapshot ?? []) {
				client.setQueryData(key, data)
			}
		},
		onSettled(): Promise<unknown> {
			return options.invalidate(client)
		},
	}
}

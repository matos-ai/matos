import { QueryClient } from '@tanstack/react-query'
import { describe, expect, it, vi } from 'vitest'
import { optimistic } from '#/lib/optimistic'

const KEY = ['a'] as const

function setup() {
	const client = new QueryClient()
	client.setQueryData(KEY, { n: 1 })
	client.setQueryData(['b'], { n: 10 })
	const invalidate = vi.fn(async () => {})
	const handlers = optimistic<{ by: number }>(client, {
		filters: () => ({ queryKey: KEY }),
		update: (data, vars) => ({ n: (data as { n: number }).n + vars.by }),
		invalidate,
	})
	return { client, invalidate, handlers }
}

describe('optimistic', () => {
	it('applies the updater at once to the matching caches only', async () => {
		const { client, handlers } = setup()
		await handlers.onMutate({ by: 5 })
		expect(client.getQueryData(KEY)).toEqual({ n: 6 })
		expect(client.getQueryData(['b'])).toEqual({ n: 10 })
	})

	it('rolls back from the snapshot on error', async () => {
		const { client, handlers } = setup()
		const context = await handlers.onMutate({ by: 5 })
		handlers.onError(new Error('x'), { by: 5 }, context)
		expect(client.getQueryData(KEY)).toEqual({ n: 1 })
	})

	it('invalidates on settle', async () => {
		const { client, invalidate, handlers } = setup()
		await handlers.onSettled()
		expect(invalidate).toHaveBeenCalledWith(client)
	})

	it('skips caches that hold no data', async () => {
		const client = new QueryClient()
		const update = vi.fn()
		const handlers = optimistic<null>(client, {
			filters: () => ({ queryKey: KEY }),
			update,
			invalidate: async () => {},
		})
		await handlers.onMutate(null)
		expect(update).not.toHaveBeenCalled()
		expect(client.getQueryData(KEY)).toBeUndefined()
	})

	it('tolerates a missing context on error', async () => {
		const { client, handlers } = setup()
		handlers.onError(new Error('x'), { by: 1 }, undefined)
		expect(client.getQueryData(KEY)).toEqual({ n: 1 })
	})
})

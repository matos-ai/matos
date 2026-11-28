import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { tanstackApi } from '#/api'
import type { Schemas } from '#/api/api.client'
import { useAccessToken } from '#/domain/identity/hooks/use-access-token'
import {
	cardWriteFilters,
	detailStructuralSharing,
	invalidateCards,
} from '#/domain/tasks/hooks/use-comment-thread'
import { optimistic } from '#/lib/optimistic'

export {
	discardFailedComment,
	type PendingComment,
	useCommentCard,
	useRetryComment,
} from '#/domain/tasks/hooks/use-comment-thread'

const LIST_ID = '/api/v1/cards'

function keyId(queryKey: readonly unknown[]): string | undefined {
	const head = queryKey[0]
	return typeof head === 'object' && head !== null
		? (head as { _id?: string })._id
		: undefined
}

function mapCard(
	data: unknown,
	queryKey: readonly unknown[],
	cardId: string,
	change: (card: Schemas.CardResponse) => Schemas.CardResponse,
): unknown {
	if (keyId(queryKey) === LIST_ID) {
		const list = data as Schemas.CardListResponse
		return Array.isArray(list.items)
			? {
					...list,
					items: list.items.map((card) =>
						card.id === cardId ? change(card) : card,
					),
				}
			: data
	}
	const detail = data as Schemas.CardDetailResponse
	return detail.card ? { ...detail, card: change(detail.card) } : data
}

export function applyCardPatch(
	card: Schemas.CardResponse,
	body: Schemas.PatchCardRequest,
): Schemas.CardResponse {
	const next = { ...card }
	if (body.title != null) next.title = body.title
	if (body.description != null) next.description = body.description
	if (body.priority != null) next.priority = body.priority
	if (body.labels != null) next.labels = body.labels
	if (body.acceptance_criteria != null) {
		next.acceptance_criteria = body.acceptance_criteria
	}
	if (body.assignee_agent_id !== undefined) {
		next.assignee_agent_id = body.assignee_agent_id
	}
	if (body.assignee_subject !== undefined) {
		next.assignee_subject = body.assignee_subject
	}
	if (body.due_date !== undefined) next.due_date = body.due_date
	if (body.estimate !== undefined) next.estimate = body.estimate
	if (body.parent_id !== undefined) next.parent_id = body.parent_id
	if (body.project_id === null) next.project = null
	if (body.client_id === null) next.client = null
	return next
}

export function useCards() {
	const accessToken = useAccessToken()
	return useQuery({
		...tanstackApi.get('/api/v1/cards', { query: {} }).queryOptions,
		enabled: Boolean(accessToken),
	})
}

export function useCard(cardId: string) {
	const accessToken = useAccessToken()
	return useQuery({
		...tanstackApi.get('/api/v1/cards/{card_id}', {
			path: { card_id: cardId },
		}).queryOptions,
		structuralSharing: detailStructuralSharing,
		enabled: Boolean(accessToken),
	})
}

export function useCreateCard() {
	const client = useQueryClient()
	return useMutation({
		...tanstackApi.mutation('post', '/api/v1/cards').mutationOptions,
		onSuccess: () => invalidateCards(client),
	})
}

export function useUpdateCard() {
	const client = useQueryClient()
	const base = tanstackApi.mutation(
		'patch',
		'/api/v1/cards/{card_id}',
	).mutationOptions
	return useMutation({
		...base,
		...optimistic<Parameters<typeof base.mutationFn>[0]>(client, {
			filters: (vars) => cardWriteFilters(vars.path.card_id),
			update: (data, vars, key) =>
				mapCard(data, key, vars.path.card_id, (card) =>
					applyCardPatch(card, vars.body),
				),
			invalidate: invalidateCards,
		}),
	})
}

export function useMoveCard() {
	const client = useQueryClient()
	const base = tanstackApi.mutation(
		'post',
		'/api/v1/cards/{card_id}/move',
	).mutationOptions
	return useMutation({
		...base,
		...optimistic<Parameters<typeof base.mutationFn>[0]>(client, {
			filters: (vars) => cardWriteFilters(vars.path.card_id),
			update: (data, vars, key) =>
				mapCard(data, key, vars.path.card_id, (card) => ({
					...card,
					column: vars.body.to,
				})),
			invalidate: invalidateCards,
		}),
	})
}

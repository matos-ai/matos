import {
	type Query,
	type QueryClient,
	type QueryFilters,
	replaceEqualDeep,
	useMutation,
	useQueryClient,
} from '@tanstack/react-query'
import { tanstackApi } from '#/api'
import type { Schemas } from '#/api/api.client'
import { optimistic } from '#/lib/optimistic'

export type PendingComment = Schemas.CommentResponse & {
	pending?: boolean
	failed?: boolean
	interrupt?: boolean
}

const CARDS_ID = '/api/v1/cards'
const DETAIL_ID = '/api/v1/cards/{card_id}'

type Detail = Omit<Schemas.CardDetailResponse, 'comments'> & {
	comments: PendingComment[]
}

function head(query: Query): { _id?: string; path?: { card_id?: string } } {
	const value: unknown = query.queryKey[0]
	return typeof value === 'object' && value !== null ? value : {}
}

export function invalidateCards(client: QueryClient) {
	return client.invalidateQueries({
		predicate: (query) => head(query)._id?.startsWith(CARDS_ID) ?? false,
	})
}

export function cardDetailFilters(cardId: string): QueryFilters {
	return {
		predicate: (query) => {
			const { _id: id, path } = head(query)
			return id === DETAIL_ID && path?.card_id === cardId
		},
	}
}

export function cardWriteFilters(cardId: string): QueryFilters {
	return {
		predicate: (query) => {
			const { _id: id, path } = head(query)
			return id === CARDS_ID || (id === DETAIL_ID && path?.card_id === cardId)
		},
	}
}

function patchComments(
	client: QueryClient,
	cardId: string,
	change: (comments: PendingComment[]) => PendingComment[],
) {
	client.setQueriesData<Detail>(cardDetailFilters(cardId), (data) =>
		data && Array.isArray(data.comments)
			? { ...data, comments: change(data.comments) }
			: data,
	)
}

function findComment(
	client: QueryClient,
	cardId: string,
	id: string,
): PendingComment | undefined {
	for (const [, data] of client.getQueriesData<Detail>(
		cardDetailFilters(cardId),
	)) {
		const found = data?.comments?.find((comment) => comment.id === id)
		if (found) return found
	}
	return undefined
}

function markFailed(client: QueryClient, cardId: string, tempId: string) {
	patchComments(client, cardId, (comments) =>
		comments.map((comment) =>
			comment.id === tempId
				? { ...comment, pending: false, failed: true }
				: comment,
		),
	)
}

function settle(
	client: QueryClient,
	cardId: string,
	tempId: string,
	posted: Schemas.PostedCommentResponse,
) {
	const server: PendingComment = {
		id: posted.id,
		author_actor: posted.author_actor,
		author_subject: posted.author_subject,
		body: posted.body,
		created_at: posted.created_at,
	}
	retire(tempId)
	patchComments(client, cardId, (comments) => {
		if (!comments.some((comment) => comment.id === tempId)) return comments
		const rest = comments.filter((comment) => comment.id !== tempId)
		if (rest.some((comment) => comment.id === posted.id)) return rest
		return comments.map((comment) => (comment.id === tempId ? server : comment))
	})
}

const retired = new Set<string>()

function retire(id: string) {
	retired.add(id)
}

function isLocal(comment: PendingComment) {
	return Boolean(comment.pending || comment.failed)
}

function knownByServer(local: PendingComment, server: PendingComment[]) {
	const sentAt = Date.parse(local.created_at)
	return server.some(
		(comment) =>
			comment.body === local.body &&
			comment.author_subject === local.author_subject &&
			Date.parse(comment.created_at) > sentAt,
	)
}

export function mergeLocalComments(old: unknown, next: unknown): unknown {
	const previous = old as Detail | undefined
	const incoming = next as Detail | undefined
	if (
		!Array.isArray(previous?.comments) ||
		!Array.isArray(incoming?.comments)
	) {
		return next
	}
	const kept = previous.comments.filter(
		(comment) =>
			isLocal(comment) &&
			!retired.has(comment.id) &&
			!incoming.comments.some((known) => known.id === comment.id) &&
			!knownByServer(comment, incoming.comments),
	)
	if (kept.length === 0) return next
	return { ...incoming, comments: [...incoming.comments, ...kept] }
}

export function detailStructuralSharing(old: unknown, next: unknown): unknown {
	return replaceEqualDeep(old, mergeLocalComments(old, next))
}

function currentSubject(client: QueryClient): string {
	const me = client.getQueryData(
		tanstackApi.get('/api/v1/me').queryOptions.queryKey,
	)
	return me?.subject ?? ''
}

function tempIdentifier(): string {
	return `pending-${crypto.randomUUID()}`
}

function threadOptimistic<TVars>(
	client: QueryClient,
	cardId: string,
	change: (comments: PendingComment[]) => PendingComment[],
) {
	return optimistic<TVars>(client, {
		filters: () => cardDetailFilters(cardId),
		update: (data) => {
			const detail = data as Detail
			return Array.isArray(detail.comments)
				? { ...detail, comments: change(detail.comments) }
				: data
		},
		invalidate: invalidateCards,
	})
}

interface CommentContext {
	tempId: string
}

export function useCommentCard() {
	const client = useQueryClient()
	const post = tanstackApi.mutation('post', '/api/v1/cards/{card_id}/comments')
	return useMutation({
		...post.mutationOptions,
		onMutate: async (vars): Promise<CommentContext> => {
			const tempId = tempIdentifier()
			const entry: PendingComment = {
				id: tempId,
				author_subject: currentSubject(client),
				body: vars.body.body,
				created_at: new Date().toISOString(),
				pending: true,
				...(vars.body.interrupt ? { interrupt: true } : {}),
			}
			await threadOptimistic<typeof vars>(client, vars.path.card_id, (c) => [
				...c,
				entry,
			]).onMutate(vars)
			return { tempId }
		},
		onSuccess: (posted, vars, context) => {
			if (context) settle(client, vars.path.card_id, context.tempId, posted)
		},
		onError: (_error, vars, context) => {
			if (context) markFailed(client, vars.path.card_id, context.tempId)
		},
		onSettled: () => invalidateCards(client),
	})
}

export interface RetryCommentVars {
	cardId: string
	tempId: string
}

export function useRetryComment() {
	const client = useQueryClient()
	const post = tanstackApi.mutation('post', '/api/v1/cards/{card_id}/comments')
	return useMutation<
		Schemas.PostedCommentResponse,
		unknown,
		RetryCommentVars,
		CommentContext
	>({
		mutationKey: [{ method: 'post', path: 'retry-comment' }],
		mutationFn: ({ cardId, tempId }) => {
			const entry = findComment(client, cardId, tempId)
			if (!entry) throw new Error('comment_not_found')
			return post.mutationOptions.mutationFn({
				path: { card_id: cardId },
				body: {
					body: entry.body,
					...(entry.interrupt ? { interrupt: true } : {}),
				},
			})
		},
		onMutate: async (vars) => {
			if (!findComment(client, vars.cardId, vars.tempId)?.failed) {
				throw new Error('comment_not_retryable')
			}
			await threadOptimistic<RetryCommentVars>(
				client,
				vars.cardId,
				(comments) =>
					comments.map((comment) =>
						comment.id === vars.tempId
							? {
									...comment,
									pending: true,
									failed: false,
									created_at: new Date().toISOString(),
								}
							: comment,
					),
			).onMutate(vars)
			return { tempId: vars.tempId }
		},
		onSuccess: (posted, vars) =>
			settle(client, vars.cardId, vars.tempId, posted),
		onError: (_error, vars, context) => {
			if (context) markFailed(client, vars.cardId, vars.tempId)
		},
		onSettled: () => invalidateCards(client),
	})
}

export function discardFailedComment(
	client: QueryClient,
	cardId: string,
	tempId: string,
): void {
	retire(tempId)
	patchComments(client, cardId, (comments) =>
		comments.filter((comment) => !(comment.id === tempId && comment.failed)),
	)
}

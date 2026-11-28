import type { Schemas } from '#/api/api.client'
import { TOO_MANY_REPOSITORIES } from '#/domain/tasks/run-agent'

export type AgentRunOutcome = Schemas.AgentRunOutcome

export const AGENT_BUSY_NOTICE =
	"Message en file : l'agent le lira à la fin de son tour."

export const CANCEL_NOTICE = {
	cancel_forbidden:
		"Seule la personne qui a lancé le run ou un admin peut l'arrêter.",
	cancel_failed: "L'arrêt a échoué. Réessaie.",
} as const

/** Ce que la personne lit quand l'arrêt a échoué ; rien quand le run était déjà fini. */
export function cancelNotice(error: unknown): string | null {
	const status = (error as { status?: number } | null)?.status
	if (status === 409) return null
	return status === 403
		? CANCEL_NOTICE.cancel_forbidden
		: CANCEL_NOTICE.cancel_failed
}

/** Ce que la personne lit quand l'agent n'a pas démarré ; rien quand il a démarré. */
export function handoffNotice(
	outcome: AgentRunOutcome | null | undefined,
): string | null {
	if (!outcome || outcome.run_id) return null
	if (outcome.skipped === 'agent_busy') return AGENT_BUSY_NOTICE
	if (
		outcome.skipped === 'cancel_forbidden' ||
		outcome.skipped === 'cancel_failed'
	)
		return CANCEL_NOTICE[outcome.skipped]
	if (outcome.skipped === 'choose_repository') return outcome.message ?? null
	if (outcome.skipped === 'too_many_repositories') {
		return `L'agent n'a pas démarré : ${TOO_MANY_REPOSITORIES}`
	}
	return `L'agent n'a pas démarré : ${outcome.message ?? 'raison inconnue'}`
}

/**
 * La création d'une tâche renvoie le démarrage de l'agent, mais la fenêtre se
 * ferme et la page de la tâche s'ouvre : elle le reprend ici, une seule fois.
 */
const pending = new Map<string, AgentRunOutcome>()

export function rememberHandoff(
	cardId: string,
	outcome: AgentRunOutcome | null | undefined,
): void {
	if (outcome) pending.set(cardId, outcome)
}

export function takeHandoff(cardId: string): AgentRunOutcome | undefined {
	const outcome = pending.get(cardId)
	pending.delete(cardId)
	return outcome
}

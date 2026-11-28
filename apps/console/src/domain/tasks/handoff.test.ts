import { describe, expect, it } from 'vitest'
import {
	cancelNotice,
	handoffNotice,
	rememberHandoff,
	takeHandoff,
} from '#/domain/tasks/handoff'

describe('handoff', () => {
	it('says nothing when the agent started', () => {
		expect(handoffNotice({ run_id: 'r1' })).toBeNull()
		expect(handoffNotice(null)).toBeNull()
	})

	it('tells a busy agent queued the message, whatever the server says', () => {
		expect(
			handoffNotice({ skipped: 'agent_busy', message: 'Déjà au travail' }),
		).toBe("Message en file : l'agent le lira à la fin de son tour.")
	})

	it('maps the cancel codes', () => {
		expect(handoffNotice({ skipped: 'cancel_forbidden' })).toBe(
			"Seule la personne qui a lancé le run ou un admin peut l'arrêter.",
		)
		expect(handoffNotice({ skipped: 'cancel_failed' })).toBe(
			"L'arrêt a échoué. Réessaie.",
		)
	})

	it('explains a failed stop by its status', () => {
		expect(cancelNotice({ status: 403 })).toBe(
			"Seule la personne qui a lancé le run ou un admin peut l'arrêter.",
		)
		expect(cancelNotice({ status: 409 })).toBeNull()
		expect(cancelNotice({ status: 500 })).toBe("L'arrêt a échoué. Réessaie.")
		expect(cancelNotice(new Error('x'))).toBe("L'arrêt a échoué. Réessaie.")
	})

	it('relays a missing repository as is', () => {
		expect(
			handoffNotice({ skipped: 'choose_repository', message: 'Choisis' }),
		).toBe('Choisis')
	})

	it('explains a refusal', () => {
		expect(
			handoffNotice({ skipped: 'no_provider', message: 'aucun fournisseur' }),
		).toBe("L'agent n'a pas démarré : aucun fournisseur")
	})

	it('explains too many repositories', () => {
		expect(handoffNotice({ skipped: 'too_many_repositories' })).toBe(
			"L'agent n'a pas démarré : Trop de dépôts : 4 dépôts supplémentaires au plus, sans doublon.",
		)
	})

	it('hands the outcome over once', () => {
		rememberHandoff('c1', { run_id: 'r1' })
		expect(takeHandoff('c1')).toEqual({ run_id: 'r1' })
		expect(takeHandoff('c1')).toBeUndefined()
	})
})

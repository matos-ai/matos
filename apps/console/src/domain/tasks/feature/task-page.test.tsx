import { act, fireEvent, screen, waitFor, within } from '@testing-library/react'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { renderAtPath } from '#/components/layout/test-router'
import { TaskPage } from '#/domain/tasks/feature/task-page'
import { cancellingStore } from '#/domain/tasks/hooks/use-cancel-run'
import {
	AGENT_LEAD,
	AGENT_SUB,
	AGENTS_ROUTE,
	card,
	jsonBody,
} from '#/domain/tasks/test-fixtures'
import { type RunSubscriptionHandlers, realtimeStore } from '#/lib/realtime'
import { clearAuth } from '#/store/auth.store'
import { mockApi, withQueryClient } from '#/test-utils/api-mock'

const realtime = vi.hoisted(() => ({
	handlers: null as RunSubscriptionHandlers | null,
}))

vi.mock('#/lib/realtime', async (importOriginal) => ({
	...(await importOriginal<typeof import('#/lib/realtime')>()),
	subscribeRun: (_runId: string, handlers: RunSubscriptionHandlers) => {
		realtime.handlers = handlers
		return () => {
			realtime.handlers = null
		}
	},
}))

const EXTRA = ['/tasks', '/tasks/$taskId', '/agents/$agentId/{-$tab}']

const ROOT = '11111111-aaaa-4000-8000-000000000001'
const KID_DONE = '22222222-bbbb-4000-8000-000000000002'
const KID_OPEN = '33333333-cccc-4000-8000-000000000003'
const PARENT = '44444444-dddd-4000-8000-000000000004'

const root = card(ROOT, {
	title: 'Tâche racine',
	description: 'Contexte détaillé',
	column: 'todo',
	assignee_agent_id: AGENT_LEAD,
	project: { id: 'p1', name: 'Site vitrine' },
	client: { id: 'c1', name: 'Acme' },
	labels: ['routine'],
	priority: 'high',
	version: 7,
})

const list = (items: ReturnType<typeof card>[]) => ({ items })

const BASE = {
	...AGENTS_ROUTE,
	[`GET /api/v1/cards/${ROOT}`]: {
		card: root,
		comments: [
			{
				id: 'cm1',
				author_subject: 'user-1',
				author_actor: 'Nathaël',
				body: 'Premier commentaire',
				created_at: new Date().toISOString(),
			},
		],
		runs: [],
	},
	'GET /api/v1/cards': list([
		root,
		card(KID_DONE, { title: 'Fille faite', column: 'done', parent_id: ROOT }),
		card(KID_OPEN, { title: 'Fille ouverte', parent_id: ROOT }),
	]),
}

afterEach(() => {
	realtimeStore.setState(() => ({ state: 'offline' }))
	cancellingStore.setState(() => ({ runIds: [] }))
	vi.unstubAllGlobals()
	clearAuth()
	document.getElementById('topbar-actions')?.remove()
})

const open = (pathId: string) =>
	renderAtPath(
		withQueryClient(<TaskPage pathId={pathId} />),
		`/tasks/${pathId}`,
		EXTRA,
	)

describe('TaskPage', () => {
	it('shows the plan waiting on the task and applies it', async () => {
		const plan = {
			id: 'ap-7',
			kind: 'repository_change',
			title: 'Plan de Lead à appliquer sur Hypolia/lobby',
			detail: {
				repository: 'Hypolia/lobby',
				base_branch: 'main',
				plan: '- Retirer Pitchout du menu',
				truncated: false,
			},
			requested_by_agent_id: AGENT_LEAD,
			card_id: ROOT,
			run_id: 'run-1',
			status: 'pending',
			decided_by: null,
			decided_at: null,
			decision_note: null,
			created_at: new Date().toISOString(),
			resumed_run_id: null,
		}
		const fetchMock = mockApi({
			...BASE,
			'GET /api/v1/approvals': {
				items: [plan, { ...plan, id: 'ap-8', card_id: KID_OPEN }],
				pending: 2,
			},
			'POST /api/v1/approvals/ap-7/decide': {
				...plan,
				status: 'approved',
				resumed_run_id: 'run-2',
				resume_error: null,
			},
		})
		await open(ROOT)
		const panel = await screen.findByRole('region', { name: 'Plan proposé' })
		expect(within(panel).getAllByRole('article')).toHaveLength(1)
		expect(within(panel).getByText('Retirer Pitchout du menu')).toBeTruthy()
		fireEvent.click(
			within(panel).getByRole('button', { name: `Appliquer : ${plan.title}` }),
		)
		expect(await within(panel).findByText(/Plan appliqué/)).toBeTruthy()
		expect(
			fetchMock.mock.calls.some(
				([url, init]) =>
					String(url).endsWith('/api/v1/approvals/ap-7/decide') &&
					init?.method === 'POST',
			),
		).toBe(true)
	})

	it('renders the card, its comments, properties and sub-tasks', async () => {
		mockApi(BASE)
		await open(ROOT)
		expect((await screen.findByRole('heading', { level: 1 })).textContent).toBe(
			'Tâche racine',
		)
		expect(screen.getAllByText('11111111').length).toBeGreaterThan(0)
		expect(screen.getByText('Contexte détaillé')).toBeTruthy()
		expect(screen.getByText('Premier commentaire')).toBeTruthy()
		expect(screen.getByText('Nathaël')).toBeTruthy()
		expect(await screen.findByText('1 / 2 terminées')).toBeTruthy()
		const props = screen.getByRole('complementary', { name: 'Propriétés' })
		expect(within(props).getByText('Haute')).toBeTruthy()
		expect(within(props).getByText('Site vitrine')).toBeTruthy()
		expect(within(props).getByText('Acme')).toBeTruthy()
		expect(within(props).getByText('routine')).toBeTruthy()
		await waitFor(() =>
			expect(within(props).getByText('Veille légale')).toBeTruthy(),
		)
		expect(
			(screen.getByRole('combobox', { name: 'Statut' }) as HTMLSelectElement)
				.value,
		).toBe('todo')
	})

	it('shows the parent banner for a sub-task', async () => {
		mockApi({
			...BASE,
			[`GET /api/v1/cards/${KID_OPEN}`]: {
				card: card(KID_OPEN, { parent_id: ROOT, title: 'Fille ouverte' }),
				comments: [],
				runs: [],
			},
		})
		await open(KID_OPEN)
		expect(await screen.findByText(/Déléguée par/)).toBeTruthy()
		expect(
			screen
				.getAllByRole('link', { name: '11111111' })[0]
				?.getAttribute('href'),
		).toBe(`/tasks/${ROOT}`)
	})

	it('shows not found for a missing card', async () => {
		mockApi({ ...AGENTS_ROUTE, 'GET /api/v1/cards': list([]) })
		await open('99999999-0000-4000-8000-000000000009')
		expect(await screen.findByText('Tâche introuvable')).toBeTruthy()
		expect(screen.getByRole('link', { name: 'Retour aux tâches' })).toBeTruthy()
	})

	it('shows an error on a server failure', async () => {
		mockApi({
			...BASE,
			[`GET /api/v1/cards/${ROOT}`]: {
				status: 500,
				body: { code: 'x', message: 'x', status: 500 },
			},
		})
		await open(ROOT)
		expect(
			await screen.findByText('Impossible de charger la tâche.'),
		).toBeTruthy()
	})

	it('moves the card with the expected version and toggles the properties', async () => {
		const slot = document.createElement('div')
		slot.id = 'topbar-actions'
		document.body.append(slot)
		const fetchMock = mockApi({
			...BASE,
			[`POST /api/v1/cards/${ROOT}/move`]: {
				...root,
				column: 'in_progress',
				version: 8,
			},
		})
		await open(ROOT)
		fireEvent.change(await screen.findByRole('combobox', { name: 'Statut' }), {
			target: { value: 'progress' },
		})
		await waitFor(() =>
			expect(jsonBody(fetchMock, 'POST', `/api/v1/cards/${ROOT}/move`)).toEqual(
				{
					to: 'in_progress',
					expected_version: 7,
				},
			),
		)
		expect(await screen.findByText('Statut : En cours')).toBeTruthy()
		fireEvent.click(screen.getByRole('button', { name: 'Fermer' }))
		expect(screen.queryByRole('combobox', { name: 'Statut' })).toBeNull()
		fireEvent.click(
			screen.getByRole('button', { name: 'Afficher les propriétés' }),
		)
		expect(screen.getByRole('combobox', { name: 'Statut' })).toBeTruthy()
	})

	it('changes the assignee through the card update', async () => {
		const fetchMock = mockApi({
			...BASE,
			[`PATCH /api/v1/cards/${ROOT}`]: {
				...root,
				assignee_agent_id: AGENT_SUB,
				version: 8,
			},
		})
		await open(ROOT)
		const select = await screen.findByRole('combobox', { name: 'Assigné à' })
		await waitFor(() =>
			expect(
				within(select)
					.getAllByRole('option')
					.map((o) => o.textContent),
			).toEqual(['Personne', 'Veille légale', 'Registre']),
		)
		expect((select as HTMLSelectElement).value).toBe(AGENT_LEAD)
		fireEvent.change(select, { target: { value: AGENT_SUB } })
		await waitFor(() =>
			expect(jsonBody(fetchMock, 'PATCH', `/api/v1/cards/${ROOT}`)).toEqual({
				assignee_agent_id: AGENT_SUB,
				expected_version: 7,
			}),
		)
		expect(await screen.findByText('Assignée à Registre')).toBeTruthy()
		await waitFor(() =>
			expect(
				fetchMock.mock.calls.filter(
					([input]) =>
						new URL(String(input)).pathname === `/api/v1/cards/${ROOT}`,
				).length,
			).toBeGreaterThan(2),
		)
	})

	it('unassigns and reports a conflict', async () => {
		const fetchMock = mockApi({
			...BASE,
			[`PATCH /api/v1/cards/${ROOT}`]: {
				status: 409,
				body: { code: 'conflict', message: 'x', status: 409 },
			},
		})
		await open(ROOT)
		fireEvent.change(
			await screen.findByRole('combobox', { name: 'Assigné à' }),
			{ target: { value: '' } },
		)
		await waitFor(() =>
			expect(jsonBody(fetchMock, 'PATCH', `/api/v1/cards/${ROOT}`)).toEqual({
				assignee_agent_id: null,
				expected_version: 7,
			}),
		)
		expect(
			await screen.findByText('Cette tâche a changé entre-temps, réessaie'),
		).toBeTruthy()
	})

	it('reports a version conflict', async () => {
		mockApi({
			...BASE,
			[`POST /api/v1/cards/${ROOT}/move`]: {
				status: 409,
				body: { code: 'conflict', message: 'x', status: 409 },
			},
		})
		await open(ROOT)
		fireEvent.change(await screen.findByRole('combobox', { name: 'Statut' }), {
			target: { value: 'done' },
		})
		expect(
			await screen.findByText('Cette tâche a changé entre-temps, réessaie'),
		).toBeTruthy()
	})

	it('only offers the statuses backed by a column', async () => {
		mockApi(BASE)
		await open(ROOT)
		const select = await screen.findByRole('combobox', { name: 'Statut' })
		expect(
			within(select)
				.getAllByRole('option')
				.map((o) => o.textContent),
		).toEqual(['En cours', 'À faire', 'Proposé', 'Terminé', 'Rejeté'])
	})

	it('posts a comment through the API', async () => {
		const fetchMock = mockApi({
			...BASE,
			[`POST /api/v1/cards/${ROOT}/comments`]: {
				status: 201,
				body: {
					id: 'cm2',
					author_subject: 'user-1',
					body: 'Bonjour',
					created_at: new Date().toISOString(),
				},
			},
		})
		await open(ROOT)
		const field = await screen.findByRole('textbox', { name: 'Message' })
		fireEvent.change(field, { target: { value: 'Bonjour' } })
		fireEvent.click(screen.getByRole('button', { name: 'Envoyer' }))
		await waitFor(() =>
			expect(
				jsonBody(fetchMock, 'POST', `/api/v1/cards/${ROOT}/comments`),
			).toEqual({ body: 'Bonjour' }),
		)
		await waitFor(() => expect((field as HTMLTextAreaElement).value).toBe(''))
	})

	it('a reply starts the agent and shows it working', async () => {
		const RUN = '99999999-eeee-4000-8000-000000000009'
		mockApi({
			...BASE,
			[`POST /api/v1/cards/${ROOT}/comments`]: {
				status: 201,
				body: {
					id: 'cm2',
					author_subject: 'user-1',
					body: 'Ajoute les montants',
					created_at: new Date().toISOString(),
					agent_run: { run_id: RUN },
				},
			},
			[`GET /api/v1/runs/${RUN}`]: {
				id: RUN,
				status: 'running',
				created_at: new Date().toISOString(),
				initiated_by: 'user-1',
				pipeline: [],
			},
			[`GET /api/v1/runs/${RUN}/events`]: {
				events: [
					{
						seq: 1,
						kind: 'tool_use',
						payload: { id: 't1', tool: 'mestier__list_projects', input: {} },
						created_at: new Date().toISOString(),
					},
				],
				next_after: null,
			},
		})
		await open(ROOT)
		const field = await screen.findByRole('textbox', { name: 'Message' })
		expect((field as HTMLTextAreaElement).placeholder).toMatch(/^Répondre à/)
		fireEvent.change(field, { target: { value: 'Ajoute les montants' } })
		fireEvent.click(screen.getByRole('button', { name: 'Envoyer' }))
		const panel = await screen.findByRole('region', {
			name: "Exécution de l'agent",
		})
		expect(await within(panel).findByText('En cours')).toBeTruthy()
		expect(within(panel).getByText('écrit…')).toBeTruthy()
		expect(
			await within(panel).findByText(/mestier__list_projects/),
		).toBeTruthy()
	})

	it('says the agent is busy when a reply lands during a run', async () => {
		mockApi({
			...BASE,
			[`POST /api/v1/cards/${ROOT}/comments`]: {
				status: 201,
				body: {
					id: 'cm2',
					author_subject: 'user-1',
					body: 'Et aussi',
					created_at: new Date().toISOString(),
					agent_run: {
						skipped: 'agent_busy',
						message: "L'agent travaille déjà sur cette tâche.",
					},
				},
			},
		})
		await open(ROOT)
		fireEvent.change(await screen.findByRole('textbox', { name: 'Message' }), {
			target: { value: 'Et aussi' },
		})
		fireEvent.click(screen.getByRole('button', { name: 'Envoyer' }))
		expect(
			await screen.findByText(
				"Message en file : l'agent le lira à la fin de son tour.",
			),
		).toBeTruthy()
	})

	it('delegates a sub-task to a sub-agent as a child card', async () => {
		const fetchMock = mockApi({
			...BASE,
			'GET /api/v1/cards': list([root]),
			'POST /api/v1/cards': {
				status: 201,
				body: card(PARENT, { parent_id: ROOT }),
			},
		})
		await open(ROOT)
		fireEvent.change(
			await screen.findByRole('textbox', { name: 'Titre de la sous-tâche' }),
			{ target: { value: 'Vérifier le SIREN' } },
		)
		await waitFor(() =>
			expect(
				within(screen.getByRole('combobox', { name: 'Sous-agent' })).getByRole(
					'option',
					{ name: 'Registre' },
				),
			).toBeTruthy(),
		)
		fireEvent.click(screen.getByRole('button', { name: 'Déléguer' }))
		await waitFor(() =>
			expect(jsonBody(fetchMock, 'POST', '/api/v1/cards')).toEqual({
				title: 'Vérifier le SIREN',
				assignee_agent_id: AGENT_SUB,
				project_id: 'p1',
				parent_id: ROOT,
			}),
		)
		expect(await screen.findByText(/déléguée à Registre/)).toBeTruthy()
	})

	it('refuses past the parallel limit and an empty title', async () => {
		const fetchMock = mockApi({
			...BASE,
			'GET /api/v1/cards': list([
				root,
				card(KID_DONE, { parent_id: ROOT }),
				card(KID_OPEN, { parent_id: ROOT }),
				card(PARENT, { parent_id: ROOT }),
				card('55555555-eeee-4000-8000-000000000005', { parent_id: ROOT }),
			]),
		})
		await open(ROOT)
		const title = await screen.findByRole('textbox', {
			name: 'Titre de la sous-tâche',
		})
		fireEvent.click(await screen.findByRole('button', { name: 'Déléguer' }))
		expect(
			await screen.findByText('Écris le titre de la sous-tâche'),
		).toBeTruthy()
		fireEvent.change(title, { target: { value: 'Encore' } })
		fireEvent.click(screen.getByRole('button', { name: 'Déléguer' }))
		expect(
			await screen.findByText('Limite atteinte : 3 sous-tâches en parallèle'),
		).toBeTruthy()
		expect(jsonBody(fetchMock, 'POST', '/api/v1/cards')).toBeUndefined()
	})

	it('refuses to delegate past the depth limit', async () => {
		mockApi({
			...BASE,
			[`GET /api/v1/cards/${KID_OPEN}`]: {
				card: card(KID_OPEN, {
					parent_id: PARENT,
					assignee_agent_id: AGENT_LEAD,
				}),
				comments: [],
				runs: [],
			},
			'GET /api/v1/cards': list([
				card('66666666-ffff-4000-8000-000000000006', {}),
				card(PARENT, { parent_id: '66666666-ffff-4000-8000-000000000006' }),
				card(KID_OPEN, { parent_id: PARENT, assignee_agent_id: AGENT_LEAD }),
			]),
		})
		await open(KID_OPEN)
		fireEvent.change(
			await screen.findByRole('textbox', { name: 'Titre de la sous-tâche' }),
			{ target: { value: 'Trop profond' } },
		)
		fireEvent.click(screen.getByRole('button', { name: 'Déléguer' }))
		expect(
			await screen.findByText('Profondeur maximale atteinte (2)'),
		).toBeTruthy()
	})

	it('lists the artifacts of the task and opens one', async () => {
		const artifact = {
			id: 'art1',
			kind: 'markdown',
			title: 'Compte rendu',
			card_id: ROOT,
			created_by: 'agent',
			created_at: new Date().toISOString(),
			updated_at: new Date().toISOString(),
		}
		const fetchMock = mockApi({
			...BASE,
			'GET /api/v1/artifacts': { items: [artifact], next_cursor: null },
			'GET /api/v1/artifacts/art1': { ...artifact, content: '## Synthèse' },
		})
		await open(ROOT)
		fireEvent.click(await screen.findByRole('button', { name: /Compte rendu/ }))
		const dialog = await screen.findByRole('dialog')
		expect(
			await within(dialog).findByRole('heading', { name: 'Synthèse' }),
		).toBeTruthy()
		const request = fetchMock.mock.calls
			.map(([input]) => new URL(String(input)))
			.find((url) => url.pathname === '/api/v1/artifacts')
		expect(request?.searchParams.get('card_id')).toBe(ROOT)
	})
})

const LIVE_RUN = '77777777-eeee-4000-8000-000000000007'
const STARTED = '2026-11-01T10:00:00.000Z'
const BEFORE = '2026-11-01T09:00:00.000Z'
const AFTER = '2026-11-01T10:05:00.000Z'

const runRow = (status: string, extra: Record<string, unknown> = {}) => ({
	id: LIVE_RUN,
	status,
	created_at: STARTED,
	started_at: STARTED,
	initiated_by: 'user-1',
	pipeline: [],
	...extra,
})

const humanComment = (id: string, body: string, at: string) => ({
	id,
	author_subject: 'user-1',
	author_actor: 'Nathaël',
	body,
	created_at: at,
})

const eventRow = (
	seq: number,
	kind: string,
	payload: Record<string, unknown>,
) => ({ seq, kind, payload, created_at: STARTED })

const working = (
	comments: unknown[] = [],
	extra: Record<string, unknown> = {},
) => ({
	...BASE,
	[`GET /api/v1/cards/${ROOT}`]: {
		card: root,
		comments,
		runs: [runRow('running')],
	},
	[`GET /api/v1/runs/${LIVE_RUN}`]: runRow('running'),
	[`GET /api/v1/runs/${LIVE_RUN}/events`]: {
		events: [
			eventRow(1, 'assistant_text', { text: 'Je commence', truncated: false }),
			eventRow(2, 'tool_use', { id: 't1', tool: 'lire', input: { p: 'a' } }),
			eventRow(3, 'tool_result', { id: 't1', succeeded: true, output: 'ok' }),
		],
		next_after: null,
	},
	[`POST /api/v1/runs/${LIVE_RUN}/cancel`]: {
		already_cancelled: false,
		status: 'cancelled',
	},
	...extra,
})

const posted = (id: string, body: string, agentRun?: unknown) => ({
	status: 201,
	body: {
		id,
		author_subject: 'user-1',
		body,
		created_at: new Date().toISOString(),
		...(agentRun ? { agent_run: agentRun } : {}),
	},
})

const callsTo = (
	fetchMock: ReturnType<typeof mockApi>,
	method: string,
	path: string,
) =>
	fetchMock.mock.calls.filter(
		([input, init]) =>
			new URL(String(input)).pathname === path &&
			(init?.method ?? 'GET') === method,
	)

describe('TaskPage live thread', () => {
	it('shows the stream of a working run, its live text and no polling when realtime is up', async () => {
		realtimeStore.setState(() => ({ state: 'connected' }))
		mockApi(working())
		await open(ROOT)
		const turn = await screen.findByRole('region', {
			name: "Exécution de l'agent",
		})
		expect(await within(turn).findByText('Je commence')).toBeTruthy()
		expect(within(turn).getByRole('button', { name: /lire/ })).toBeTruthy()
		await waitFor(() => expect(realtime.handlers).not.toBeNull())
		act(() => {
			realtime.handlers?.onSubscribed()
			realtime.handlers?.onDelta(3, 'texte en train de venir')
		})
		expect(
			await within(turn).findByText('texte en train de venir'),
		).toBeTruthy()
		expect(turn.getAttribute('aria-busy')).toBe('true')
	})

	it('offers to go back down when the reader scrolled up during a run', async () => {
		mockApi(working())
		await open(ROOT)
		const turn = await screen.findByRole('region', {
			name: "Exécution de l'agent",
		})
		const scroller = turn.closest('.overflow-auto') as HTMLElement
		expect(screen.queryByRole('button', { name: 'Aller en bas' })).toBeNull()
		for (const [key, value] of Object.entries({
			scrollHeight: 2000,
			clientHeight: 500,
			scrollTop: 100,
		})) {
			Object.defineProperty(scroller, key, {
				configurable: true,
				writable: true,
				value,
			})
		}
		act(() => {
			fireEvent.scroll(scroller)
		})
		fireEvent.click(await screen.findByRole('button', { name: 'Aller en bas' }))
		await waitFor(() => expect(scroller.scrollTop).toBe(2000))
	})

	it('stops the run from the turn or the composer', async () => {
		const fetchMock = mockApi(working())
		await open(ROOT)
		await screen.findByRole('region', { name: "Exécution de l'agent" })
		const stops = await screen.findAllByRole('button', { name: 'Arrêter' })
		expect(stops).toHaveLength(2)
		fireEvent.click(stops[1])
		await waitFor(() =>
			expect(
				callsTo(fetchMock, 'POST', `/api/v1/runs/${LIVE_RUN}/cancel`),
			).toHaveLength(1),
		)
		fireEvent.click(screen.getAllByRole('button', { name: 'Arrêter' })[0])
		await waitFor(() =>
			expect(
				callsTo(fetchMock, 'POST', `/api/v1/runs/${LIVE_RUN}/cancel`),
			).toHaveLength(2),
		)
	})

	it('stops with the keyboard shortcut', async () => {
		const fetchMock = mockApi(working())
		await open(ROOT)
		await screen.findAllByRole('button', { name: 'Arrêter' })
		fireEvent.keyDown(window, { key: '.', metaKey: true })
		await waitFor(() =>
			expect(
				callsTo(fetchMock, 'POST', `/api/v1/runs/${LIVE_RUN}/cancel`),
			).toHaveLength(1),
		)
	})

	it('shows the cancelling state', async () => {
		mockApi(working())
		cancellingStore.setState(() => ({ runIds: [LIVE_RUN] }))
		await open(ROOT)
		const buttons = await screen.findAllByRole('button', {
			name: 'Arrêt en cours…',
		})
		expect(buttons).toHaveLength(2)
		for (const button of buttons) {
			expect((button as HTMLButtonElement).disabled).toBe(true)
		}
		expect(screen.getAllByText('Arrêt en cours…').length).toBeGreaterThan(0)
	})

	it('keeps a refusal to stop on screen', async () => {
		mockApi(
			working([], {
				[`POST /api/v1/runs/${LIVE_RUN}/cancel`]: {
					status: 403,
					body: { code: 'cancel_forbidden', message: 'non', status: 403 },
				},
			}),
		)
		await open(ROOT)
		fireEvent.click(
			(await screen.findAllByRole('button', { name: 'Arrêter' }))[0],
		)
		const alert = await screen.findByRole('alert')
		expect(alert.textContent).toContain(
			"Seule la personne qui a lancé le run ou un admin peut l'arrêter.",
		)
	})

	it('refreshes without a message when the run already finished', async () => {
		const fetchMock = mockApi(
			working([], {
				[`POST /api/v1/runs/${LIVE_RUN}/cancel`]: {
					status: 409,
					body: { code: 'run_already_finished', message: 'fini', status: 409 },
				},
			}),
		)
		await open(ROOT)
		await screen.findByRole('region', { name: "Exécution de l'agent" })
		const before = callsTo(fetchMock, 'GET', `/api/v1/runs/${LIVE_RUN}`).length
		fireEvent.click(
			(await screen.findAllByRole('button', { name: 'Arrêter' }))[0],
		)
		await waitFor(() =>
			expect(
				callsTo(fetchMock, 'GET', `/api/v1/runs/${LIVE_RUN}`).length,
			).toBeGreaterThan(before),
		)
		expect(screen.queryByRole('alert')).toBeNull()
	})

	it('queues a message while the run works and sends with interrupt on demand', async () => {
		const fetchMock = mockApi(
			working([], {
				[`POST /api/v1/cards/${ROOT}/comments`]: posted('cm9', 'Suite', {
					skipped: 'agent_busy',
				}),
			}),
		)
		await open(ROOT)
		const field = await screen.findByRole('textbox', { name: 'Message' })
		await screen.findAllByRole('button', { name: 'Arrêter' })
		expect(
			screen.queryByRole('button', { name: 'Arrêter et envoyer' }),
		).toBeNull()
		fireEvent.change(field, { target: { value: 'Suite' } })
		const both = screen.getByRole('button', { name: 'Arrêter et envoyer' })
		expect(both.getAttribute('aria-keyshortcuts')).toBe(
			'Control+Shift+Enter Meta+Shift+Enter',
		)
		fireEvent.click(screen.getByRole('button', { name: 'Envoyer' }))
		await waitFor(() =>
			expect(
				jsonBody(fetchMock, 'POST', `/api/v1/cards/${ROOT}/comments`),
			).toEqual({ body: 'Suite' }),
		)
		expect(
			await screen.findByText(
				"Message en file : l'agent le lira à la fin de son tour.",
			),
		).toBeTruthy()
	})

	it('sends with interrupt from the button and adopts the new run', async () => {
		const NEXT = '88888888-eeee-4000-8000-000000000008'
		const fetchMock = mockApi(
			working([], {
				[`POST /api/v1/cards/${ROOT}/comments`]: posted('cm9', 'Change', {
					run_id: NEXT,
				}),
				[`GET /api/v1/runs/${NEXT}`]: runRow('running', { id: NEXT }),
			}),
		)
		await open(ROOT)
		const field = await screen.findByRole('textbox', { name: 'Message' })
		fireEvent.change(field, { target: { value: 'Change' } })
		fireEvent.click(
			await screen.findByRole('button', { name: 'Arrêter et envoyer' }),
		)
		await waitFor(() =>
			expect(
				jsonBody(fetchMock, 'POST', `/api/v1/cards/${ROOT}/comments`),
			).toEqual({ body: 'Change', interrupt: true }),
		)
		await waitFor(() =>
			expect(
				callsTo(fetchMock, 'GET', `/api/v1/runs/${NEXT}`).length,
			).toBeGreaterThan(0),
		)
	})

	it('wires the send shortcuts', async () => {
		const fetchMock = mockApi(
			working([], {
				[`POST /api/v1/cards/${ROOT}/comments`]: posted('cm9', 'x'),
			}),
		)
		await open(ROOT)
		const field = await screen.findByRole('textbox', { name: 'Message' })
		await screen.findAllByRole('button', { name: 'Arrêter' })
		fireEvent.change(field, { target: { value: 'un' } })
		fireEvent.keyDown(field, { key: 'Enter', metaKey: true })
		await waitFor(() =>
			expect(
				callsTo(fetchMock, 'POST', `/api/v1/cards/${ROOT}/comments`),
			).toHaveLength(1),
		)
		fireEvent.change(field, { target: { value: 'deux' } })
		fireEvent.keyDown(field, { key: 'Enter', metaKey: true, shiftKey: true })
		await waitFor(() =>
			expect(
				callsTo(fetchMock, 'POST', `/api/v1/cards/${ROOT}/comments`),
			).toHaveLength(2),
		)
		const bodies = callsTo(
			fetchMock,
			'POST',
			`/api/v1/cards/${ROOT}/comments`,
		).map(([, init]) => JSON.parse(String(init?.body)))
		expect(bodies).toEqual([{ body: 'un' }, { body: 'deux', interrupt: true }])
	})

	it('does nothing special on Escape in an empty composer', async () => {
		const fetchMock = mockApi(working())
		await open(ROOT)
		const field = await screen.findByRole('textbox', { name: 'Message' })
		fireEvent.keyDown(field, { key: 'Escape' })
		expect(
			callsTo(fetchMock, 'POST', `/api/v1/runs/${LIVE_RUN}/cancel`),
		).toHaveLength(0)
	})

	const failingSend = async () => {
		const fetchMock = mockApi({
			...BASE,
			[`POST /api/v1/cards/${ROOT}/comments`]: {
				status: 500,
				body: { code: 'boom', message: 'boom', status: 500 },
			},
		})
		await open(ROOT)
		const field = await screen.findByRole('textbox', { name: 'Message' })
		fireEvent.change(field, { target: { value: 'Perdu' } })
		fireEvent.click(screen.getByRole('button', { name: 'Envoyer' }))
		expect(await screen.findByText('Non envoyé')).toBeTruthy()
		expect(screen.getByText('Perdu')).toBeTruthy()
		return fetchMock
	}

	it('marks a failed message and lets one retry it', async () => {
		const fetchMock = await failingSend()
		fireEvent.click(screen.getByRole('button', { name: 'Réessayer' }))
		await waitFor(() =>
			expect(
				callsTo(fetchMock, 'POST', `/api/v1/cards/${ROOT}/comments`),
			).toHaveLength(2),
		)
	})

	it('lets one discard a failed message', async () => {
		await failingSend()
		fireEvent.click(screen.getByRole('button', { name: 'Supprimer' }))
		await waitFor(() => expect(screen.queryByText('Perdu')).toBeNull())
		expect(screen.queryByText('Non envoyé')).toBeNull()
	})

	it('marks only the messages written after the run started as queued', async () => {
		mockApi(
			working([
				humanComment('old', 'Avant le run', BEFORE),
				humanComment('new', 'Pendant le run', AFTER),
			]),
		)
		await open(ROOT)
		await screen.findByText('Pendant le run')
		expect(
			screen.getAllByText("En file : l'agent le lira à la fin de son tour"),
		).toHaveLength(1)
	})

	it('shows run details under the final comment, loaded only when opened', async () => {
		const done = runRow('succeeded', {
			finished_at: '2026-11-01T10:01:00.000Z',
		})
		const fetchMock = mockApi({
			...BASE,
			[`GET /api/v1/cards/${ROOT}`]: {
				card: root,
				comments: [
					{
						id: 'final',
						author_subject: 'agent',
						author_actor: 'agent:veille-légale-(lead)',
						body: 'Voici le résultat final',
						created_at: '2026-11-01T10:00:50.000Z',
					},
					humanComment('after', 'Merci', AFTER),
				],
				runs: [done],
			},
			[`GET /api/v1/runs/${LIVE_RUN}/events`]: {
				events: [
					eventRow(1, 'tool_use', { id: 't', tool: 'chercher', input: {} }),
					eventRow(2, 'tool_result', {
						id: 't',
						succeeded: true,
						output: 'ok',
					}),
				],
				next_after: null,
			},
		})
		await open(ROOT)
		const toggle = await screen.findByRole('button', { name: /Détails du run/ })
		const items = screen
			.getAllByRole('listitem')
			.map((item) => item.textContent ?? '')
		const finalAt = items.findIndex((t) =>
			t.includes('Voici le résultat final'),
		)
		const detailsAt = items.findIndex((t) => t.includes('Détails du run'))
		const mercisAt = items.findIndex((t) => t.includes('Merci'))
		expect(detailsAt).toBeGreaterThan(finalAt)
		expect(detailsAt).toBeLessThan(mercisAt)
		expect(
			callsTo(fetchMock, 'GET', `/api/v1/runs/${LIVE_RUN}/events`),
		).toHaveLength(0)
		fireEvent.click(toggle)
		expect(await screen.findByRole('button', { name: /chercher/ })).toBeTruthy()
		expect(
			callsTo(fetchMock, 'GET', `/api/v1/runs/${LIVE_RUN}/events`),
		).toHaveLength(1)
		expect(screen.getAllByText('Voici le résultat final')).toHaveLength(1)
	})
})

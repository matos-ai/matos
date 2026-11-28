import { ChevronRight } from 'lucide-react'
import { useEffect, useState } from 'react'
import type { Schemas } from '#/api/api.client'
import { InlineAlert } from '#/components/layout/inline-alert'
import {
	useRefreshCard,
	useRunPolling,
} from '#/domain/tasks/hooks/use-agent-run'
import { useIsCancelling } from '#/domain/tasks/hooks/use-cancel-run'
import { useRunStream } from '#/domain/tasks/hooks/use-run-stream'
import { isActiveRun, RUN_STATUS_LABEL } from '#/domain/tasks/run-agent'
import { AgentTurn, StopButton, TurnHeader } from '#/domain/tasks/ui/agent-turn'
import { useRealtimeState } from '#/hooks/use-realtime'
import { cn } from '#/lib/utils'

interface Agent {
	name: string
	hue: number
}

interface Props {
	runId: string
	agent: Agent
	onActiveChange: (active: boolean) => void
	onStop: () => void
	stopNotice: string | null
}

export function RunStatusPanel({
	runId,
	agent,
	onActiveChange,
	onStop,
	stopNotice,
}: Props) {
	const connected = useRealtimeState() === 'connected'
	const run = useRunPolling(runId, !connected)
	const refresh = useRefreshCard()
	const cancelling = useIsCancelling(runId)
	const status = run.data?.status
	const active = status === undefined || isActiveRun(status)
	const finished = !active
	const stream = useRunStream(runId, active)

	useEffect(() => {
		if (finished) {
			refresh()
			onActiveChange(false)
		}
	}, [finished, refresh, onActiveChange])

	if (run.isError) {
		return (
			<InlineAlert tone="danger">
				Impossible de suivre l'exécution de l'agent.
			</InlineAlert>
		)
	}
	if (finished && run.data) {
		return (
			<ul className="m-0 list-none p-0">
				<RunDetails run={run.data} agent={agent} />
			</ul>
		)
	}
	if (finished) return null
	return (
		<div className="grid gap-2">
			<AgentTurn
				active
				events={stream.events}
				live={stream.live}
				avatar={agent}
				header={
					<TurnHeader
						agent={agent}
						active
						status={
							status ? RUN_STATUS_LABEL[status] : 'Chargement de l’exécution…'
						}
						actions={
							<StopButton cancelling={cancelling} onStop={onStop} showLabel />
						}
					/>
				}
			/>
			{stopNotice ? (
				<InlineAlert tone="danger" className="ml-10">
					{stopNotice}
				</InlineAlert>
			) : null}
		</div>
	)
}

export function RunDetails({
	run,
	agent,
}: {
	run: Schemas.RunResponse
	agent: Agent
}) {
	const [open, setOpen] = useState(false)
	const [opened, setOpened] = useState(false)
	const stream = useRunStream(opened ? run.id : null, false)
	const reason =
		(run.status === 'failed' || run.status === 'cancelled') && run.failure
			? run.failure
			: null
	const toggle = () => {
		setOpen(!open)
		setOpened(true)
	}
	return (
		<li className="grid gap-1.5 pl-10">
			<button
				type="button"
				aria-expanded={open}
				onClick={toggle}
				className="flex w-fit items-center gap-1.5 rounded-md px-1.5 py-0.5 text-[13px] text-muted-foreground hover:bg-secondary hover:text-foreground"
			>
				<ChevronRight
					aria-hidden="true"
					className={cn(
						'size-3.5 transition-transform motion-reduce:transition-none',
						open && 'rotate-90',
					)}
				/>
				Détails du run
				<span aria-hidden="true">·</span>
				<span>{RUN_STATUS_LABEL[run.status]}</span>
			</button>
			{reason ? (
				run.status === 'failed' ? (
					<InlineAlert tone="danger">{reason}</InlineAlert>
				) : (
					<p className="m-0 text-[13px] text-muted-foreground">{reason}</p>
				)
			) : null}
			{open ? (
				stream.loading ? (
					<p className="m-0 text-[13px] text-muted-foreground">Chargement…</p>
				) : stream.events.length === 0 ? (
					<p className="m-0 text-[13px] text-muted-foreground">
						Aucun détail enregistré.
					</p>
				) : (
					<AgentTurn
						active={false}
						events={stream.events}
						live=""
						header={
							<TurnHeader agent={agent} active={false} status="Détail du run" />
						}
					/>
				)
			) : null}
		</li>
	)
}

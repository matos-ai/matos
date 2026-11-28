import { Check, ChevronRight, Loader2, Square, X } from 'lucide-react'
import { type ReactNode, useState } from 'react'
import { Markdown } from '#/components/markdown'
import type { RunStreamEvent } from '#/domain/tasks/run-stream'
import { AgentAvatar } from '#/domain/tasks/ui/agent-chip'
import {
	foldValue,
	type ToolStatus,
	type TurnBlock,
	turnBlocks,
} from '#/domain/tasks/ui/agent-turn-model'
import { cn } from '#/lib/utils'

const STATUS_LABEL: Record<ToolStatus, string> = {
	pending: 'En cours',
	ok: 'Réussi',
	failed: 'Échec',
}

const STOP_SHORTCUT = 'Control+. Meta+.'

export function StopButton({
	cancelling,
	onStop,
	showLabel = false,
}: {
	cancelling: boolean
	onStop: () => void
	showLabel?: boolean
}) {
	const label = cancelling ? 'Arrêt en cours…' : 'Arrêter'
	return (
		<button
			type="button"
			aria-label={label}
			aria-keyshortcuts={STOP_SHORTCUT}
			title={`${label} (⌘ .)`}
			disabled={cancelling}
			onClick={onStop}
			className={cn(
				'inline-flex items-center gap-1.5 rounded-md border text-[13px] font-medium text-foreground hover:bg-secondary disabled:opacity-60',
				showLabel ? 'h-7 px-2.5' : 'size-8 justify-center rounded-full',
			)}
		>
			{cancelling ? (
				<Loader2 aria-hidden="true" className="size-3.5 animate-spin" />
			) : (
				<Square aria-hidden="true" className="size-3.5 fill-current" />
			)}
			{showLabel ? label : null}
		</button>
	)
}

export function TurnHeader({
	agent,
	status,
	active,
	actions,
}: {
	agent: { name: string; hue: number }
	status: string
	active: boolean
	actions?: ReactNode
}) {
	return (
		<div className="flex min-h-7 flex-wrap items-center gap-x-2 gap-y-1 text-[14px]">
			<b className="font-semibold">{agent.name}</b>
			<span className="text-muted-foreground">{status}</span>
			{active ? (
				<span className="text-[13px] text-muted-foreground">écrit…</span>
			) : null}
			{actions ? <span className="ml-auto">{actions}</span> : null}
		</div>
	)
}

function FoldedValue({ label, value }: { label: string; value: string }) {
	const [all, setAll] = useState(false)
	const { shown, folded } = foldValue(value)
	if (value === '') return null
	return (
		<div className="grid gap-1">
			<span className="text-[11.5px] font-medium text-muted-foreground">
				{label}
			</span>
			<pre className="m-0 max-h-none overflow-x-auto rounded-md border bg-secondary/50 p-2 font-mono text-[11.5px] leading-snug whitespace-pre-wrap break-words">
				{all ? value : shown}
			</pre>
			{folded ? (
				<button
					type="button"
					onClick={() => setAll(!all)}
					className="justify-self-start rounded-md px-1.5 py-0.5 text-[12px] font-medium text-muted-foreground hover:bg-secondary hover:text-foreground"
				>
					{all ? 'Réduire' : 'Afficher tout'}
				</button>
			) : null}
		</div>
	)
}

function StatusMark({ status }: { status: ToolStatus }) {
	return (
		<span
			className="inline-flex shrink-0 items-center"
			title={STATUS_LABEL[status]}
		>
			{status === 'pending' ? (
				<Loader2
					aria-hidden="true"
					className="size-3.5 animate-spin text-muted-foreground motion-reduce:animate-none"
				/>
			) : status === 'ok' ? (
				<Check aria-hidden="true" className="size-3.5 text-signal-success" />
			) : (
				<X aria-hidden="true" className="size-3.5 text-signal-danger" />
			)}
			<span className="sr-only">{STATUS_LABEL[status]}</span>
		</span>
	)
}

function ToolRow({ block }: { block: Extract<TurnBlock, { type: 'tool' }> }) {
	const [open, setOpen] = useState(false)
	return (
		<li className="min-w-0 rounded-md border bg-card/60">
			<button
				type="button"
				aria-expanded={open}
				onClick={() => setOpen(!open)}
				className="flex w-full min-w-0 items-center gap-2 rounded-md px-2.5 py-1.5 text-left text-[13px] hover:bg-secondary"
			>
				<ChevronRight
					aria-hidden="true"
					className={cn(
						'size-3.5 shrink-0 text-muted-foreground transition-transform motion-reduce:transition-none',
						open && 'rotate-90',
					)}
				/>
				<span className="shrink-0 font-mono text-[12.5px] font-medium">
					{block.tool}
				</span>
				<span className="min-w-0 flex-1 truncate font-mono text-[12px] text-muted-foreground">
					{block.summary}
				</span>
				<StatusMark status={block.status} />
			</button>
			{open ? (
				<div className="grid gap-2 border-t px-2.5 py-2">
					<FoldedValue label="Entrée" value={block.input} />
					<FoldedValue label="Sortie" value={block.output ?? ''} />
				</div>
			) : null}
		</li>
	)
}

function BlockView({ block }: { block: TurnBlock }) {
	if (block.type === 'text') {
		return (
			<li className="min-w-0 text-[14.5px] leading-relaxed">
				<Markdown source={block.text} />
				{block.truncated ? (
					<p className="m-0 mt-1 text-[12px] text-muted-foreground">
						texte tronqué
					</p>
				) : null}
			</li>
		)
	}
	if (block.type === 'system') {
		return (
			<li className="m-0 text-[12.5px] text-muted-foreground">{block.text}</li>
		)
	}
	return <ToolRow block={block} />
}

export function AgentTurn({
	events,
	live,
	active,
	header,
	avatar,
}: {
	events: ReadonlyArray<RunStreamEvent>
	live: string
	active: boolean
	header?: ReactNode
	avatar?: { hue: number }
}) {
	const blocks = turnBlocks(events)
	return (
		<section
			aria-label="Exécution de l'agent"
			aria-live="polite"
			aria-busy={active}
			className={cn(
				'grid gap-x-3',
				avatar ? 'grid-cols-[28px_minmax(0,1fr)]' : 'grid-cols-[minmax(0,1fr)]',
			)}
		>
			{avatar ? <AgentAvatar hue={avatar.hue} size={28} /> : null}
			<div className="min-w-0">
				{header}
				{blocks.length > 0 || live ? (
					<div className="mt-1 grid gap-2">
						{blocks.length > 0 ? (
							<ul className="m-0 grid list-none gap-2 p-0">
								{blocks.map((block) => (
									<BlockView key={block.key} block={block} />
								))}
							</ul>
						) : null}
						{live ? (
							<div
								aria-live="off"
								className={cn(
									'min-w-0 text-[14.5px] leading-relaxed',
									active &&
										"[&_p:last-child]:after:ml-1 [&_p:last-child]:after:inline-block [&_p:last-child]:after:h-[1.1em] [&_p:last-child]:after:w-[2px] [&_p:last-child]:after:animate-pulse [&_p:last-child]:after:bg-muted-foreground [&_p:last-child]:after:align-middle [&_p:last-child]:after:content-[''] motion-reduce:[&_p:last-child]:after:animate-none",
								)}
							>
								<Markdown source={live} />
							</div>
						) : null}
					</div>
				) : null}
			</div>
		</section>
	)
}

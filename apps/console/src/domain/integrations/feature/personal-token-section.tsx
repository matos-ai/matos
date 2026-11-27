import { ExternalLink } from 'lucide-react'
import { type FormEvent, useState } from 'react'
import type { Schemas } from '#/api/api.client'
import { InlineAlert } from '#/components/layout/inline-alert'
import { Button } from '#/components/ui/button'
import { Field } from '#/components/ui/field'
import { Input } from '#/components/ui/input'
import { Switch } from '#/components/ui/switch'
import {
	describePutTokenError,
	describeTokenActionError,
} from '#/domain/integrations/github-errors'
import {
	useDeleteMyGithubToken,
	usePutMyGithubToken,
	useSetMyGithubTokenActive,
} from '#/domain/integrations/hooks/use-github'
import { ConfirmDialog } from '#/domain/routines/ui/confirm-dialog'
import { cn } from '#/lib/utils'

const TOKEN_URL = 'https://github.com/settings/personal-access-tokens/new'

const DATE = new Intl.DateTimeFormat('fr-FR', {
	day: 'numeric',
	month: 'short',
	year: 'numeric',
})

function formatDate(value: string): string {
	const date = new Date(value)
	return Number.isNaN(date.getTime()) ? value : DATE.format(date)
}

function isExpired(value: string): boolean {
	const time = new Date(value).getTime()
	return !Number.isNaN(time) && time <= Date.now()
}

interface TokenFormProps {
	onCancel?: (() => void) | undefined
	onSaved: () => void
}

function TokenForm({ onCancel, onSaved }: TokenFormProps) {
	const put = usePutMyGithubToken()
	const [value, setValue] = useState('')

	const submit = (event: FormEvent) => {
		event.preventDefault()
		const token = value.trim()
		if (token === '') return
		put.mutate(token, {
			onSuccess: () => {
				setValue('')
				onSaved()
			},
		})
	}

	return (
		<form onSubmit={submit} className="grid max-w-[520px] gap-2" noValidate>
			<Field label="Jeton GitHub">
				<Input
					type="password"
					value={value}
					autoComplete="off"
					spellCheck={false}
					placeholder="github_pat_…"
					onChange={(event) => setValue(event.target.value)}
				/>
			</Field>
			{put.isError ? (
				<InlineAlert tone="danger">
					{describePutTokenError(put.error)}
				</InlineAlert>
			) : null}
			<div className="flex gap-2">
				<Button type="submit" disabled={put.isPending || value.trim() === ''}>
					Enregistrer
				</Button>
				{onCancel ? (
					<Button type="button" variant="ghost" onClick={onCancel}>
						Annuler
					</Button>
				) : null}
			</div>
		</form>
	)
}

interface SavedTokenProps {
	token: Schemas.GithubPersonalTokenResponse
	onNotice: (message: string) => void
}

function SavedToken({ token, onNotice }: SavedTokenProps) {
	const setActive = useSetMyGithubTokenActive()
	const remove = useDeleteMyGithubToken()
	const [replacing, setReplacing] = useState(false)
	const [confirming, setConfirming] = useState(false)
	const expiresAt = token.expires_at ?? null
	const expired = expiresAt !== null && isExpired(expiresAt)

	return (
		<div className="grid gap-2">
			<div className="flex flex-wrap items-center justify-between gap-2">
				<p className="m-0">
					Jeton de <b className="font-semibold">@{token.login}</b>{' '}
					<span className="font-mono text-muted-foreground">
						(…{token.preview})
					</span>
					{expiresAt !== null ? (
						<span
							className={cn(
								'ml-1',
								expired
									? 'font-medium text-signal-attention-ink'
									: 'text-muted-foreground',
							)}
						>
							{expired
								? `· expiré le ${formatDate(expiresAt)}`
								: `· expire le ${formatDate(expiresAt)}`}
						</span>
					) : null}
				</p>
				<div className="flex gap-2">
					<Button
						size="sm"
						variant="outline"
						onClick={() => setReplacing((value) => !value)}
					>
						Remplacer
					</Button>
					<Button
						size="sm"
						variant="outline"
						disabled={remove.isPending}
						onClick={() => setConfirming(true)}
					>
						Supprimer
					</Button>
				</div>
			</div>
			{expired ? (
				<InlineAlert tone="danger">
					Ce jeton a expiré : GitHub le refusera. Remplace-le ou supprime-le.
				</InlineAlert>
			) : null}
			<Switch
				label="Utiliser ce jeton"
				description={
					token.active
						? "Utilisé à la place de l'autorisation via l'application."
						: undefined
				}
				checked={token.active}
				disabled={setActive.isPending}
				onCheckedChange={(active) =>
					setActive.mutate(active, {
						onError: () => onNotice(describeTokenActionError()),
					})
				}
			/>
			{replacing ? (
				<TokenForm
					onCancel={() => setReplacing(false)}
					onSaved={() => {
						setReplacing(false)
						onNotice('Jeton personnel remplacé')
					}}
				/>
			) : null}
			<ConfirmDialog
				open={confirming}
				title="Supprimer ton jeton personnel ?"
				description="Les pull requests et les pushes repasseront par l'autorisation via l'application GitHub."
				confirmLabel="Supprimer"
				onOpenChange={setConfirming}
				onConfirm={() => {
					remove.mutate(undefined, {
						onSuccess: () => onNotice('Jeton personnel supprimé'),
						onError: () => onNotice(describeTokenActionError()),
					})
					setConfirming(false)
				}}
			/>
		</div>
	)
}

interface Props {
	token: Schemas.GithubPersonalTokenResponse | null
	onNotice: (message: string) => void
}

export function PersonalTokenSection({ token, onNotice }: Props) {
	return (
		<section className="mt-4 grid gap-2 border-t pt-4">
			<h3 className="m-0 text-sm font-semibold">Jeton personnel (optionnel)</h3>
			<p className="m-0 max-w-[620px] text-muted-foreground">
				Avec un jeton personnel, GitHub affiche la pull request et le push comme
				les tiens, sans la mention « with l'application ». Il faut un jeton
				fine-grained avec « Contents: write » et « Pull requests: write » sur
				les dépôts concernés.{' '}
				<a
					href={TOKEN_URL}
					target="_blank"
					rel="noreferrer"
					className="inline-flex items-center gap-1 text-primary"
				>
					Créer un jeton
					<ExternalLink aria-hidden="true" className="size-3.5" />
				</a>
			</p>
			{token ? (
				<SavedToken token={token} onNotice={onNotice} />
			) : (
				<TokenForm onSaved={() => onNotice('Jeton personnel enregistré')} />
			)}
		</section>
	)
}

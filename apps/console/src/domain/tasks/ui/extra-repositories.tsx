import { useId } from 'react'
import { Checkbox } from '#/components/ui/checkbox'
import { Field } from '#/components/ui/field'
import {
	extraCandidates,
	MAX_EXTRA_REPOSITORIES,
	toggleExtra,
} from '#/domain/tasks/run-agent'

interface Props {
	grants: ReadonlyArray<string>
	primary: string
	value: ReadonlyArray<string>
	onChange: (value: ReadonlyArray<string>) => void
}

export function ExtraRepositories({ grants, primary, value, onChange }: Props) {
	const prefix = useId()
	const candidates = extraCandidates(grants, primary)
	if (grants.length < 2 || candidates.length === 0) return null
	const full = value.length >= MAX_EXTRA_REPOSITORIES
	return (
		<Field
			label="Autres dépôts à modifier"
			optional
			group
			hint={`${MAX_EXTRA_REPOSITORIES} au plus. L'agent reçoit tous les dépôts côte à côte et ouvre une pull request par dépôt modifié.`}
		>
			{candidates.map((name, index) => {
				const checked = value.includes(name)
				return (
					<Checkbox
						key={name}
						id={`${prefix}-${index}`}
						label={<span className="font-mono">{name}</span>}
						checked={checked}
						disabled={full && !checked}
						onCheckedChange={(next) =>
							onChange(toggleExtra(value, name, next === true))
						}
					/>
				)
			})}
		</Field>
	)
}

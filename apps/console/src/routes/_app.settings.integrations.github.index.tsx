import { createFileRoute } from '@tanstack/react-router'
import type { GithubReturn } from '#/domain/integrations/feature/github-callback-pages'
import { GithubPage } from '#/domain/integrations/feature/github-page'

const BANNERS: Record<GithubReturn, string> = {
	app: 'Application GitHub créée. Installe-la maintenant sur GitHub.',
	installation: 'GitHub est connecté à l’organisation.',
}

export const Route = createFileRoute('/_app/settings/integrations/github/')({
	validateSearch: (search): { retour?: GithubReturn; compte?: string } =>
		typeof search.compte === 'string' && search.compte !== ''
			? { compte: search.compte }
			: search.retour === 'app' || search.retour === 'installation'
				? { retour: search.retour }
				: {},
	component: GithubRoute,
})

function GithubRoute() {
	const { retour, compte } = Route.useSearch()
	const banner = compte
		? `Compte GitHub connecté : @${compte}`
		: retour
			? BANNERS[retour]
			: undefined
	return <GithubPage banner={banner} />
}

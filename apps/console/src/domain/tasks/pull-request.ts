const PULL_REQUEST = /https:\/\/github\.com\/([\w.-]+)\/([\w.-]+)\/pull\/(\d+)/g

export interface PullRequestLink {
	url: string
	repository: string
	number: number
}

/** Les pull requests GitHub distinctes qu'un commentaire d'exécution cite, dans l'ordre. */
export function pullRequestLinks(body: string): PullRequestLink[] {
	const seen = new Set<string>()
	const links: PullRequestLink[] = []
	for (const match of body.matchAll(PULL_REQUEST)) {
		if (seen.has(match[0])) continue
		seen.add(match[0])
		links.push({
			url: match[0],
			repository: `${match[1]}/${match[2]}`,
			number: Number(match[3]),
		})
	}
	return links
}

/** La première pull request GitHub qu'un commentaire d'exécution cite. */
export function pullRequestLink(body: string): PullRequestLink | null {
	return pullRequestLinks(body)[0] ?? null
}

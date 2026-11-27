# Spec : une exécution sur plusieurs dépôts

Statut : en cours. Taille L, une PR.

## Objectif

Une exécution de type `change` modifie un dépôt principal et, en plus, jusqu'à 4 dépôts choisis au lancement. L'agent les a tous côte à côte, les modifie et les teste ensemble. Chaque dépôt modifié reçoit la même branche et sa propre pull request en brouillon, qui cite les autres.

## État actuel (vérifié dans le code)

- Une exécution vise un seul dépôt : `RepositoryTarget`, `RunSubject::Card { repository, base_branch }`, `runs.repository`, `TaskSpec.repository`, `TaskGrant.repository`, `GIT_REMOTE`.
- Les compagnons (`.matos.toml`, `companion.rs`) sont clonés à côté, en lecture seule (proxy en mode `Audit`, `pushurl` neutralisé par `companions.sh`).
- Le proxy n'accepte un push que vers `refs/heads/{grant.branch}` du dépôt principal (`pktline::check_push`).
- Le marqueur `pushed` ne porte que `sha`. L'URL de la PR est dans `run_steps.output.pull_request` et la console la lit dans le texte du commentaire de la carte.

## Décisions

| # | Sujet | Décision |
|---|---|---|
| D1 | Vocabulaire | « Dépôts supplémentaires » (`extra_repositories`) : modifiables, choisis au lancement. Les compagnons restent en lecture seule et viennent de `.matos.toml`. |
| D2 | Limite | 4 dépôts supplémentaires au plus, distincts du principal et entre eux. Supplémentaires + compagnons ≤ `MAX_COMPANIONS` (8). Un dépôt à la fois supplémentaire et compagnon est traité comme supplémentaire. |
| D3 | Droits | Chaque dépôt supplémentaire doit être dans `agent.repository_grants` (refus `RepositoryNotGranted(repo)`) et accessible à l'installation GitHub. |
| D4 | Persistance | `runs.extra_repositories TEXT[]` (`owner/name`). `RunSubject::Card` gagne `extra_repositories: Vec<RepositoryRef>`. Une réponse sur la carte (`Handoff::Replied`) reprend ceux du run précédent ; un plan appliqué (`repository_change`) les porte dans son détail. |
| D5 | Branche de base | Branche par défaut de chaque dépôt supplémentaire, lue à la préparation et gardée dans la sortie de l'étape pour la reprise. |
| D6 | Emplacement | `/workspace/<name>` ; si deux dépôts ont le même nom, `/workspace/<owner>-<name>`. Si un compagnon de `.matos.toml` déclare ce dépôt, son `path` est gardé. |
| D7 | Grant et proxy | `task_grants.writable_repositories TEXT[]`. Le proxy traite le principal et les supplémentaires en mode `Change` (push vers `refs/heads/{grant.branch}` seulement, identité membre comprise) ; les compagnons restent en lecture seule. |
| D8 | Executor | Variable `EXTRA_REPOSITORIES`, une ligne par dépôt : `owner/name\tpath\tbase_branch\tmax_files\tmax_lines\tallow_paths` (`allow_paths` séparés par des virgules, vide possible). Garde de chaque dépôt : celle de son propre `.matos.toml` (lu sur sa branche de base), défauts sinon. Clone complet comme le principal, `git switch -c "$BRANCH"`. |
| D9 | Tout ou rien | Après l'agent : formatage puis contrôle de la garde sur chaque dépôt modifié. Si un seul est bloqué, rien n'est poussé (`guard_blocked`, raisons préfixées par `owner/name: `). Sinon, lint, commit et push de chaque dépôt modifié ; un dépôt sans changement n'est ni poussé ni ouvert. |
| D10 | Marqueurs | `pushed` gagne `repository` (`owner/name`), absent = principal. `quality` gagne `repository` de la même façon. `no_changes` n'est émis que si aucun dépôt n'a changé. |
| D11 | Texte | `/workspace/.matos/contribution.md` sert à tous les dépôts. Optionnel : `/workspace/.matos/contribution/<owner>__<name>.md` pour un dépôt précis (même format), émis avec un marqueur de contribution portant `repository`. |
| D12 | Pull requests | Le worker vérifie la tête de branche de chaque dépôt poussé, ouvre une PR par dépôt (ordre : principal puis supplémentaires), puis ajoute à chaque corps une section `## Pull requests liées` listant les autres, par mise à jour du corps (`CodeHost::update_pull_request_body`). Même identité Git pour tous les dépôts : membre seulement si `member_can_push` est vrai pour chacun, sinon App (note D8 de la spec identité). |
| D13 | Reprise | La sortie de l'étape garde une table `pull_requests: {owner/name: url}` ; une PR déjà ouverte n'est jamais rouverte. `pull_request` (principal, ou premier ouvert) reste pour la compatibilité. |
| D14 | Commentaire | Le commentaire de fin liste toutes les PR, une par ligne (« Pull request ouverte en brouillon sur owner/name : url »). |
| D15 | Audit | En mode `audit`, les dépôts supplémentaires sont clonés en lecture seule, comme des compagnons. |
| D16 | Console | Au lancement (dialogue d'exécution d'agent, nouvelle tâche), sélection multiple « Autres dépôts à modifier » parmi les `repository_grants` restants. Le fil de la carte affiche un lien par PR. |

## API

- `POST /api/v1/cards/{id}/agent-runs`, `POST /api/v1/cards/{id}/executions`, `POST /api/v1/cards` (création avec agent) : champ optionnel `extra_repositories: string[]`.
- 422 `too_many_repositories` au-delà de 4 ou en cas de doublon ; refus `repository_not_granted` comme aujourd'hui.
- La sortie de l'étape `execute` (timeline) expose `pull_requests` en plus de `pull_request`.

## Acceptance

1. Un run avec un dépôt supplémentaire modifié dans les deux dépôts pousse la même branche dans les deux et ouvre deux PR, chacune listant l'autre.
2. Un dépôt supplémentaire sans changement n'a ni push ni PR ; seul le principal en a une, sans section de PR liées.
3. Une garde bloquée sur un seul dépôt n'en pousse aucun ; les raisons nomment le dépôt.
4. Le proxy accepte le push sur un dépôt supplémentaire vers la branche du run, le refuse vers une autre branche, et refuse tout push vers un compagnon.
5. Un dépôt supplémentaire hors `repository_grants` refuse le lancement ; plus de 4 donnent 422.
6. Reprise après crash entre deux ouvertures : la PR déjà ouverte n'est pas rouverte, la suivante l'est, et les deux sont liées.
7. Une réponse sur la carte relance avec les mêmes dépôts supplémentaires.
8. En identité membre, un dépôt où le membre ne peut pas pousser fait passer tout le run en identité App.
9. Le commentaire de la carte et la console listent chaque PR.

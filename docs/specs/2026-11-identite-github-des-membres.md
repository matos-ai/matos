# Spec : la pull request et le push au nom du membre

Statut : en cours. Taille L, une PR. Remplace la décision D2 de #2 (« la pull request est ouverte par la GitHub App »).

## Objectif

Une exécution qui modifie un dépôt pousse sa branche et ouvre sa pull request avec l'identité GitHub du membre qui l'a lancée, ou qui a appliqué son plan. La GitHub App reste le repli.

## État actuel (vérifié dans le code)

- L'auteur des commits est déjà le membre (`execute_pipeline.rs` `commit_author`, nom et email de `members`), sinon `<slug>[bot]`.
- Le push passe par le proxy (`apps/egress-proxy/src/git.rs`), qui injecte un jeton d'installation de l'App.
- La pull request est ouverte par le worker avec un jeton d'installation (`CodeHost::open_draft_pr`).
- L'App demande déjà une autorisation utilisateur à l'installation (`request_oauth_on_install`), mais le jeton utilisateur sert une fois puis est jeté (`UserToken`, `callback.rs`).

## Décisions

| # | Sujet | Décision |
|---|---|---|
| D1 | Qui | Le membre `run.initiated_by`, le même que l'auteur des commits. |
| D2 | Connexion | Chaque membre connecte son compte GitHub une fois, par l'autorisation utilisateur de la GitHub App de son organisation. Pas de nouvelle App, pas de PAT. |
| D3 | Retour de GitHub | Même URL de retour que l'installation (`callback_urls` du manifeste) : la console distingue par l'absence d'`installation_id` et la finalité du `state` signé. Aucune App existante à reconfigurer. |
| D4 | Stockage | Table `github_user_connections`, PK `(organization_id, subject)`. Jeton d'accès et jeton de rafraîchissement scellés avec `SECRETS_KEY`, AD `github-user/{org}/{subject}/{access|refresh}`. Login et id GitHub en clair. |
| D5 | Rafraîchissement | Le worker seul rafraîchit, au moment de préparer l'exécution : le jeton d'accès doit rester valable au moins `EXECUTOR_DEADLINE_SECS` + 10 min. Le proxy ne fait que lire (rôle en `SELECT`). Un jeton sans expiration (App sans jetons expirants) est utilisé tel quel. |
| D6 | Décision d'identité | Prise une fois par le worker à la préparation et inscrite dans `task_grants.git_identity` (`member` ou `app`). Le proxy et l'ouverture de la PR la suivent. |
| D7 | Push | Pour `git_identity = member`, le proxy authentifie `git-receive-pack` (info/refs et push) avec le jeton du membre. Le clone et le fetch restent sur le jeton d'installation en lecture seule. |
| D8 | Repli | Pas de connexion, rafraîchissement refusé, ou membre sans accès en écriture au dépôt : identité `app`, comme aujourd'hui, et le commentaire de fin sur la carte ajoute : « Pull request ouverte par l'application GitHub : connecte ton compte GitHub dans Matos pour qu'elle soit ouverte à ton nom. » avec le lien vers la page de connexion. Le run n'échoue jamais pour ça. |
| D9 | Échec tardif | Si l'ouverture de la PR avec le jeton du membre échoue (401/403/422 hors « déjà ouverte »), le worker réessaie une fois avec l'App, puis applique la note de D8. |
| D10 | Déconnexion | `DELETE` supprime la ligne et révoque l'autorisation chez GitHub (`DELETE /applications/{client_id}/grant`), sans échouer si GitHub refuse. |
| D11 | Accès | Toutes les routes sont sous `/api/v1/me/github` : un membre ne voit et ne touche que sa propre connexion. |
| D12 | Jeton personnel | GitHub affiche « with <App> » sur toute action faite avec un jeton utilisateur d'App. Pour l'éviter, un membre peut enregistrer un jeton personnel fine-grained (table `github_personal_tokens`, scellé avec `SECRETS_KEY`, AD `github-pat/{org}/{subject}`). Validé à l'enregistrement par `GET /user` (login, id) ; l'expiration vient de l'en-tête `github-authentication-token-expiration` si présent. |
| D13 | Priorité | Jeton personnel actif et non expiré, sinon autorisation via l'App, sinon App (note D8). Le worker et le proxy appliquent le même ordre. Un jeton personnel n'est jamais rafraîchi. Le membre peut le désactiver sans le supprimer. |

## API

- `GET /api/v1/me/github` : `{connected: bool, login?: string, connected_at?: string}`.
- `GET /api/v1/me/github/authorize` : `{url}` vers `{web_base}/login/oauth/authorize?client_id=…&state=…`. `state` signé, finalité `UserConnect`, 10 min. 409 si l'organisation n'a pas de GitHub App.
- `POST /api/v1/me/github/callback` `{code, state}` : échange le code, lit `GET /user`, enregistre. 400 si `state` invalide.
- `DELETE /api/v1/me/github` : 204.
- `GET /api/v1/me/github` renvoie aussi `personal_token: {login, preview, expires_at, active} | null`.
- `PUT /api/v1/me/github/token` `{token}` : valide auprès de GitHub puis enregistre, actif. 400 si GitHub refuse le jeton.
- `PATCH /api/v1/me/github/token` `{active}` : active ou désactive. 404 sans jeton.
- `DELETE /api/v1/me/github/token` : 204.

## Console

- Page Paramètres › Intégrations › GitHub : bloc « Ton compte GitHub » visible de tout membre (connecté en tant que `login`, ou bouton « Connecter mon compte GitHub »), bouton « Déconnecter ».
- La route de retour `/settings/integrations/github/callback` sans `installation_id` appelle `POST /api/v1/me/github/callback`, puis revient sur la page GitHub avec un message.

## Acceptance

1. Un membre connecté lance une exécution : la branche est poussée et la PR ouverte avec son jeton (fake GitHub : l'en-tête `Authorization` des deux appels est son jeton).
2. Un membre non connecté : push et PR avec le jeton d'installation, et le commentaire de la carte contient la note de D8.
3. Un jeton d'accès expiré avec un jeton de rafraîchissement valide est rafraîchi à la préparation, et la nouvelle paire est stockée.
4. Un rafraîchissement refusé donne l'identité `app` et la note D8, sans échec du run.
5. La PR refusée avec le jeton du membre est ouverte par l'App, avec la note D8.
6. Le proxy ne pousse avec le jeton du membre que pour `git_identity = member`, et ne l'utilise jamais pour le fetch.
7. Les jetons ne sont jamais en clair en base ni dans les logs (Debug masqué).
8. Un membre ne lit ni ne supprime la connexion d'un autre.
9. `deploy/postgres/egress-proxy-role.sql` donne `SELECT` sur `github_user_connections` et `github_personal_tokens`.
10. Un jeton personnel actif est utilisé pour le push et la PR, même si l'autorisation via l'App existe ; désactivé ou expiré, l'autorisation via l'App reprend.
11. Un jeton refusé par GitHub à l'enregistrement n'est pas stocké.

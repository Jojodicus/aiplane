# STATUS: llm-generated, unreviewed — pending native-speaker QA

tokens-page-heading = Jetons API
tokens-intro = Jetons pour les applications et le code qui se connectent à AIplane. La valeur complète n’est affichée qu’à la création ou au renouvellement — copiez-la et gardez-la secrète.

tokens-create-heading = Créer un jeton
tokens-name-label = Nom
tokens-name-placeholder = ex. laptop, ci-runner
tokens-ttl-label = Durée de vie (jours)
tokens-create-submit = Créer un jeton

tokens-list-heading = Vos jetons
tokens-list-empty = Aucun jeton pour l'instant. Créez-en un avec le bouton ci-dessus.

tokens-badge-revoked = révoqué
tokens-badge-active = actif
tokens-remove-button = Supprimer
tokens-rotate-button = Régénérer
tokens-rotate-title = Émettre un nouveau secret pour ce jeton (conserve son nom et ses paramètres)
tokens-revoke-button = Révoquer

tokens-row-meta = créé le { $created } · dernière utilisation { $last_used } · expire le { $expires }
tokens-last-used-never = jamais

tokens-tool-use-aria = Utilisation des outils
tokens-tool-use-label = Utilisation des outils

tokens-mcp-allow-description = Les outils de connecteur nécessitant une approbation ne peuvent pas demander de confirmation via l'API ; l'activation les exécute sans demander.

tokens-minted-heading = Jeton créé
tokens-minted-copy-warning = Copiez la valeur maintenant — vous ne pourrez plus la revoir ensuite.
tokens-copy-aria = Copier le jeton
tokens-minted-name = Nom : { $name }

tokens-account-user-id-label = ID utilisateur

tokens-mcp-ask-enabled-toast = Outils MCP « ask » via l'API activés pour ce jeton.
tokens-mcp-ask-disabled-toast = Outils MCP « ask » via l'API désactivés pour ce jeton.

# Web Push "turn complete" opt-in card (rendered by `render_push_card`; wired
# client-side by `ui/ts/push.ts`). Device-local notification settings.
tokens-push-enable = Activer sur cet appareil
tokens-push-disable = Désactiver sur cet appareil
tokens-push-on = Les notifications sont activées pour cet appareil.
tokens-push-enabled = Notifications activées sur cet appareil.
tokens-push-disabled = Notifications désactivées sur cet appareil.
tokens-push-error = Impossible de modifier les paramètres de notification.

# Utilisation, liste de modèles autorisés et quota par jeton (/tokens).
tokens-usage-line = ce mois-ci : { $requests } requêtes · { $tokens } tokens · { $cost }
tokens-models-summary-all = Modèles : tous
tokens-models-summary-restricted = Modèles : { $count } sélectionnés
tokens-models-help = Désactivé, ce jeton suit votre propre accès, y compris les modèles ajoutés plus tard. Activé, il ne peut utiliser que les modèles cochés — un modèle ajouté ensuite reste bloqué tant que vous ne l'avez pas coché ici aussi.
tokens-models-restrict-label = Limiter ce jeton à des modèles précis
tokens-models-save = Enregistrer les modèles
tokens-models-saved-toast = Jeton limité à { $count } modèles.
tokens-models-cleared-toast = Le jeton peut utiliser tous vos modèles.
tokens-limits-add = Ajouter un quota
tokens-limits-saved-toast = Quota du jeton enregistré.

# SPA-only: the Svelte /tokens row panels and their confirm prompts.
tokens-quota-max-placeholder = max
tokens-revoke-confirm = Révoquer ce jeton ? Les clients qui l'utilisent cessent de fonctionner immédiatement.
tokens-rotate-confirm = Émettre un nouveau secret ? L'ancien cesse de fonctionner immédiatement.
tokens-remove-confirm = Supprimer définitivement cette ligne de jeton ?

tokens-create-description = Créer un nouveau jeton Bearer pour l'API compatible OpenAI.
tokens-tool-use-description = Autoriser ce jeton à appeler les outils de la passerelle (recherche web, RAG, …).
tokens-capabilities-summary = Capacités
tokens-panel-close = Fermer
tokens-edit-button = Modifier
tokens-mcp-allow-aria = Autoriser les outils MCP en mode « ask » via l'API
tokens-mcp-allow-label = Autoriser les outils MCP « ask » via l'API
tokens-account-heading = Compte
tokens-signed-in-as = Connecté en tant que { $email }
tokens-account-oidc-label = Rôles OIDC
tokens-account-rbac-label = IDs de rôle RBAC
tokens-roles-none = aucun
tokens-roles-none-granted = aucun accordé
tokens-push-heading = Notifications
tokens-push-description = Recevez une notification sur cet appareil lorsqu'une réponse que vous avez lancée se termine pendant que vous n'êtes pas dans l'application.
tokens-push-off = Les notifications sont désactivées pour cet appareil.
tokens-push-denied = Ce navigateur a bloqué les notifications. Autorisez-les dans les paramètres du navigateur pour les activer.
tokens-push-unsupported = Ce navigateur ne prend pas en charge les notifications.
tokens-models-none-picked = Cochez au moins un modèle, ou désactivez la limite.
tokens-limits-summary-none = Quota : aucun
tokens-limits-summary-some = Quota : { $count } règle(s)
tokens-limits-help = Un plafond pour ce seul jeton. Votre propre budget s'applique toujours : ceci ne peut que restreindre la dépense du jeton, jamais l'élargir.
tokens-limits-remove = Supprimer
tokens-limits-removed-toast = Quota du jeton supprimé.
tokens-limits-admin-badge = défini par l'administrateur
tokens-models-admin-set = Un opérateur restreint aussi ce jeton à : { $models }. Votre sélection ne peut que réduire cela, pas l'élargir.

# Client setup guides and token-page tabs.
tokens-tab-tokens = Jetons
tokens-tab-guides = Guides de configuration
tokens-tab-account = Compte
tokens-guides-heading = Configurer un client
tokens-guides-intro = Choisissez une application ci-dessous et suivez les étapes. Un jeton peut servir dans plusieurs applications, mais un jeton par application est plus facile à gérer.
tokens-guides-before = Créez d’abord un jeton dans l’onglet Jetons et copiez-le. Sa valeur complète n’est affichée qu’une fois. Gardez-la secrète.
tokens-guide-opencode = OpenCode
tokens-guide-claude = Claude Code
tokens-guide-python = Python (OpenAI)
tokens-opencode-step-1 = Créez un jeton dans l’onglet Jetons et copiez-le.
tokens-opencode-step-2 = Dans OpenCode, lancez /connect, choisissez Other, saisissez aiplane comme identifiant du fournisseur, puis collez votre jeton.
tokens-opencode-step-3 = Enregistrez cette configuration dans ~/.config/opencode/opencode.jsonc. Remplacez YOUR_MODEL_ID par l’identifiant d’un modèle de chat accessible.
tokens-opencode-finish = Lancez opencode, puis choisissez aiplane/YOUR_MODEL_ID avec /models.
tokens-claude-step-1 = Créez un jeton dans l’onglet Jetons et copiez-le.
tokens-claude-step-2 = Choisissez l’identifiant d’un modèle de chat accessible. Votre administrateur a peut-être créé un alias pour Claude Code.
tokens-claude-step-3 = Collez ces commandes dans votre terminal. Remplacez d’abord le jeton et l’identifiant du modèle.
tokens-claude-finish = Claude Code utilise l’URL de base sans /v1. Si le modèle est introuvable, demandez à votre administrateur quel alias utiliser.
tokens-python-step-1 = Créez un jeton dans l’onglet Jetons et copiez-le.
tokens-python-step-2 = Installez le paquet Python OpenAI avec python -m pip install openai, puis définissez OPENAI_API_KEY avec votre jeton dans le terminal.
tokens-python-step-3 = Enregistrez cet exemple dans chat.py. Remplacez YOUR_MODEL_ID par l’identifiant d’un modèle de chat accessible.
tokens-python-finish = Exécutez python chat.py. Gardez le jeton dans une variable d’environnement, pas dans le script.
tokens-guides-model-note = Besoin d’un identifiant de modèle ? Les modèles accessibles sont dans le sélecteur du chat ou via GET /v1/models avec votre jeton.

# STATUS: llm-generated, unreviewed — pending native-speaker QA

memory-heading = Mémoire
memory-description = Ce que l'assistant retient à votre sujet, regroupé par type. Ajoutez, modifiez ou supprimez des entrées ici — c'est la mémoire de votre compte et vous la contrôlez entièrement. Activez ou désactivez la fonctionnalité elle-même sur la page Outils.

memory-add-heading = Ajouter un souvenir
memory-kind-aria = Type de souvenir
memory-content-placeholder = p. ex. Préfère les réponses en unités métriques

memory-empty = Rien ici pour l'instant.
memory-save-button = Enregistrer
memory-delete-title = Supprimer le souvenir

# SPA-only: the Svelte /memory page's inline add/edit form.
memory-kind-preference = Préférences
memory-kind-project = Contexte du projet
memory-kind-fact = Faits
memory-content-label = Contenu
memory-add-button = Mémoriser
memory-delete-confirm = Supprimer ce souvenir ?

# The per-category Add button in each card header; it opens the add dialog
# with that card's kind already selected.
memory-add-short = Ajouter

# One line under each card heading saying how that kind reaches the
# assistant: preferences ride in the system context of every conversation,
# while project notes and facts wait to be looked up with `recall`. The
# difference changes which bucket a user files something in, and nothing
# else on the page reveals it.
memory-kind-preference-hint = Envoyées avec chaque conversation tant que la mémoire est activée, pour que l'assistant les applique sans qu'on le lui demande. Les plus récentes d'abord, jusqu'à { $count } préférences et environ { $chars } caractères ; la plus récente est toujours incluse.
memory-kind-project-hint = Consulté au besoin — l'assistant va chercher ces entrées quand la conversation touche à votre travail.
memory-kind-fact-hint = Consulté au besoin — l'assistant va chercher ces entrées dès qu'elles deviennent pertinentes.
memory-preference-not-in-context = La mémoire est désactivée ou ne vous est pas accordée : ces préférences ne sont donc pas envoyées à l'assistant. Activez la mémoire dans Outils, ou demandez l'accès à un administrateur.

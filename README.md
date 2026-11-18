# ADE

Environnement de développement agentique minimaliste, écrit en Rust avec [iced](https://iced.rs).
L'agent est **Claude Code**, piloté en mode non interactif : ADE utilise donc votre **abonnement Claude**
(Pro ou Max), sans clé API.
ADE sert uniquement à dialoguer avec Claude : il n'ouvre ni n'édite aucun fichier lui-même,
c'est l'agent qui lit et modifie le projet (les modifications s'affichent en diff dans le chat).

```
┌──────────────┬──────────────────────────────────────────────────┐
│ projets      │ conversation avec Claude                         │
│              │ outils, diffs, autorisations                     │
│              │                                                  │
└──────────────┴──────────────────────────────────────────────────┘
```

## Lancer

```sh
cargo run --release -- chemin/vers/projet   # l'argument est optionnel
```

## Icône et raccourci Windows

L'icône (`assets/ade.ico`, régénérable avec `assets/make-icon.ps1`) est intégrée à `ade.exe` par `build.rs`.
Les raccourcis « ADE » du Bureau et du menu Démarrer pointent vers `target\release\ade.exe` :
après une modification du code, relancez `cargo build --release` pour les mettre à jour.

## Dossier de travail

Au premier lancement, ADE demande votre dossier de travail. Chacun de ses sous-dossiers apparaît
comme un projet dans la colonne de gauche ; cliquer dessus le sélectionne pour l'agent. Chaque projet
garde sa conversation tant qu'ADE reste ouvert. « Changer de dossier de travail » en bas de la liste.

## Abonnement Claude (Claude Code)

ADE a besoin de [Claude Code](https://docs.anthropic.com/en/docs/claude-code/setup) installé et connecté
à votre compte Claude. Si Claude Code est introuvable, ADE affiche un écran de configuration :

1. **Installer** : lance l'installeur officiel (`irm https://claude.ai/install.ps1 | iex` dans PowerShell) ;
2. **Se connecter** : ouvre Claude Code dans un terminal pour se connecter avec son compte Claude (`/login`).

Cliquez sur l'indicateur « ● Claude Code … » en haut pour revenir à cet écran.

Pour chaque message, ADE lance dans le dossier du projet :

```
claude -p --input-format stream-json --output-format stream-json --verbose
       --include-partial-messages --permission-mode bypassPermissions [--resume <session>]
```

- `ANTHROPIC_API_KEY` est retirée de l'environnement de Claude Code, pour que l'abonnement soit utilisé
  même si cette variable est définie.
- Chaque projet a sa session Claude Code, reprise avec `--resume` (le contexte est conservé par Claude Code).
  « Nouvelle conversation » démarre une nouvelle session.
- « Stop » termine Claude Code et tout ce qu'il a lancé.
- L'usage est décompté des limites de l'abonnement ; les compteurs de tokens restent affichés pour information.

## Outils de l'agent

Ce sont ceux de Claude Code (`Read`, `Edit`, `Write`, `Bash`, `Grep`, `Glob`, `WebFetch`…), exécutés
**sans demande de confirmation** (`bypassPermissions`). Les éditions s'affichent en diff dans le chat.

## Structure

- `src/main.rs`  : interface iced (état, messages, vues)
- `src/agent.rs` : lancement de Claude Code et lecture de ses événements stream-json
- `src/sessions.rs` : sauvegarde des conversations (session Claude Code + transcription)
- `src/projects.rs` : liste et création des projets (sous-dossiers du dossier de travail)
- `src/config.rs` : dossier de travail (`config.json`), détection / installation / connexion de Claude Code

# ADE

Environnement de développement agentique minimaliste, écrit en Rust avec [iced](https://iced.rs).
L'agent est piloté par Claude (`claude-opus-5-5`) via l'API Messages.
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

## Clé API

Au premier lancement, ADE demande une clé API Claude (à créer sur
[platform.claude.com/settings/keys](https://platform.claude.com/settings/keys), lien disponible dans l'écran).
La clé est vérifiée (appel gratuit à `/v1/models`) puis enregistrée dans
`%APPDATA%\ADE\config.json` (Windows) ou `~/.config/ADE/config.json`.
Cliquez sur l'indicateur « ● Clé API … » en haut pour la changer.

La variable d'environnement `ANTHROPIC_API_KEY`, si elle est définie, est prioritaire sur le fichier.

## Outils de l'agent

| Outil         | Effet                                                        |
|---------------|--------------------------------------------------------------|
| `list_dir`    | liste un dossier                                             |
| `read_file`   | lit un fichier (plage de lignes optionnelle)                 |
| `search`      | recherche regex dans le projet (respecte `.gitignore`)       |
| `write_file`  | crée / remplace un fichier, diff affiché dans le chat        |
| `edit_file`   | remplacement exact d'un extrait, diff affiché dans le chat   |
| `run_command` | commande shell (`cmd /C` ou `sh -c`), **autorisation requise** |

Tous les chemins sont confinés à la racine du projet (`..`, chemins absolus et liens symboliques sortants refusés).

## Structure

- `src/main.rs`  : interface iced (état, messages, vues)
- `src/agent.rs` : boucle agentique, streaming SSE, historique de conversation
- `src/tools.rs` : définitions et exécution des outils
- `src/projects.rs` : liste des projets (sous-dossiers du dossier de travail)
- `src/config.rs` : clé API et dossier de travail (`config.json`)

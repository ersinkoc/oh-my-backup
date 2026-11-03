# Oh My Backup

<img src="assets/logo.svg" width="96" alt="Oh My Backup logo">

A Windows desktop app for monitoring remote folders and pulling changes over SSH, with dedicated Veeam backup support. Written in Rust with egui. Turkish and English are available from the language selector and Settings.

## Use

Run the self-contained `install.ps1` at the repository root, or extract a release ZIP and run its `install.ps1`. Without a bundled executable, the installer downloads the latest stable release from `ersinkoc/oh-my-backup`. It always installs in `%LOCALAPPDATA%/Programs/OhMyBackup`, without administrator access, and creates desktop/Start menu shortcuts. Rerunning it updates that same folder. Existing startup registration is redirected to the stable executable without enabling startup for new users. Pin the new shortcut; an old pin can retain Windows' cached icon until replaced.

```powershell
./install.ps1
# Or install a locally built executable, without a GitHub download:
./install.ps1 -SourceExe ./target/release/oh-my-backup.exe
```

Close an already-running installed copy before replacing it. Reinstalling an identical binary preserves the previous-version backup. A bootstrap or bundled installer keeps a newer installed version instead of downgrading it. Use `-NoDesktopShortcut`, `-NoStartMenu` or `-Restart` when needed. The legacy `scripts/install.ps1` entry point forwards to the root installer.

## Updates inside the app

Open **Updates**, then **Check for updates**. A newer stable release can be downloaded and verified while backups continue. **Update and restart** is enabled only for the fixed-folder installation when no transfer is active. It shuts down the engine, flushes statistics, exits, replaces the executable and restarts the application. A job that races the restart request is cancelled through its normal checkpoints before exit. Installation preserves settings, credentials and backup destinations.

The updater verifies the SHA-256 checksum, GitHub's asset digest when available, archive paths and the executable's `--version` output. Automatic downgrades, links/junctions, unexpected archive entries and competing installers are rejected. The monotonic version check runs again under the installation lock. Replacement or installed-version verification failures restore the previous EXE; the previous working file is retained as `oh-my-backup.previous.exe`. This rollback covers installation failures, not arbitrary crashes after a successful restart. Explicit manual `-SourceExe` installation can restore an older version. Update results and prepared files stay in `%LOCALAPPDATA%/OhMyBackup/updates`.

For private-repository access, use an existing `gh auth login` session or `GH_TOKEN`/`GITHUB_TOKEN`. Alternatively, save a GitHub token with **Contents: read** for this repository in the Updates window. Tokens saved by the app are encrypted with DPAPI and are sent to the helper through standard input, never command-line arguments or temporary request files. No credential is bundled in the installer. A missing or expired credential produces an actionable error. The client requires Windows PowerShell 5.1, included with supported Windows installations.

The network integration uses the official [GitHub Releases asset API](https://docs.github.com/en/rest/releases/assets) and optional [GitHub CLI authentication](https://cli.github.com/manual/gh_auth_token).

`oh-my-backup.exe --check-update` performs a read-only release check without opening the GUI.

Open Settings and enter your server, SSH username/password, remote folder and local backup destination. New installations contain no server credentials and do not start automatic transfers. The SSH server needs Python 3 and a Linux `/proc` filesystem. The local filesystem must support the file operations used by the selected update mode; NTFS is recommended.

The first connection records the server's SSH key fingerprint. Verify the fingerprint through a trusted channel before relying on the connection. Changed keys are rejected. Saved passwords use Windows DPAPI for the current Windows user.

## Profiles and file rules

Use **Profiles** to add independent **Files** or **Veeam** profiles. Each has its own connection, destination, schedule, password, logs and statistics. New profiles start paused with blank connection credentials. The original configuration remains the primary profile, with its existing Veeam settings and destination unchanged. A fresh installation starts with the Files preset. Language and application-update credentials belong to the primary profile.

For Files, configure the SSH source and destination in Settings, then open **Rules**:

- Recursively scan a remote directory, or watch only its immediate files. The scan interval is configurable; this is polling, not an instant remote event subscription.
- Include/exclude paths with `*`, `**` and `?` (`**/*.pdf` also matches root-level PDFs). Patterns use `/`; matching is case-sensitive. Default exclusions cover temporary, partial and lock files.
- Wait until size and nanosecond modification time remain unchanged for the quiet period. The source is checked again before publication. This heuristic does not provide database or application-consistent snapshots.
- Stage and hash-verify downloads before atomic replacement. Keep 1–100 previous versions (default 5) in `.hbsync/generic/versions`. Local files changed outside the app and files without ownership records are protected by default.
- Keep source-deleted files by default; optionally quarantine them or explicitly enable mirror deletion. Only files recorded as managed by this application can be removed. Deletion also observes quiet periods, schedules and allowed hours. Quarantine is retained until manually cleaned up.
- Preview the next actions from the latest scan without changing files. Copy a rules-only JSON preset without including connection details or credentials. Recent execution history is shown below the editor.

Destinations must be separate across profiles; nested/overlapping targets are rejected. Symlinks/junctions, Windows-reserved names, case collisions and unsafe paths stop the scan. An incomplete scan cannot authorize deletions. Changing a source with existing managed data requires a separate target directory. Archives, hashes and ownership metadata remain on the same destination volume; keep enough free space for staging, retained versions and the configured reserve.

The profile catalog is stored beside the original settings as `config.json.profiles/catalog.json` (the full config filename is preserved for custom `HBSYNC_CONFIG` paths). Archiving a profile stops its workers and removes its catalog entry; it retains settings and destination data. The primary migrated profile cannot be archived.

Veeam retains its existing chain selection, job-safety checks and update modes. Generic file rules do not replace those backup-chain semantics. Bidirectional sync, remote writes, arbitrary executable macros, webhooks and additional storage protocols are future work.

Use `cargo run --locked -- --preview-files` for an offline sample of the Files dashboard; `--preview` shows the Veeam sample. Neither starts transfers.

## Behavior

- Block-level delta downloads and resumable checkpoints avoid downloading unchanged data.
- In-place mode minimizes writes. The local live set can be incomplete while its full-backup file is updated. Safe mode prepares a separate set and needs more disk space.
- Continuous, every-N-days, weekday and manual plans support per-backup overrides and allowed hours.
- Job windows are estimated from backup filenames and timestamps; server file activity is also checked. Estimates are not a substitute for restore testing.
- Missing or truncated local files invalidate the current-state indicator and are scheduled for repair, subject to pause/exclusion and server safety windows. Local size checks do not detect same-size bit corruption.
- Approved legacy folders may be deleted before downloading when the free-space-first option is enabled. Review approvals carefully.
- Exiting during a transfer prompts to stop and checkpoint the operation first.
- Only one production sync process is allowed, including `--once`. An offline `--preview` process can run independently.

For compatibility with existing installations, settings remain in `%APPDATA%/HBSync`, logs retain the HBSync naming, and per-destination metadata remains in `.hbsync`. `HBSYNC_CONFIG` can select a different configuration file. Existing settings are preserved; old settings without a language default to Turkish. Log entries retain the language used when they were created.

## Development

The repository pins Rust 1.93.0. Use the Windows MSVC target and Windows SDK resource compiler. `RC` may point to `rc.exe`. Python 3 is used by the source privacy check. No Python installation is needed on the client for normal desktop operation.

```powershell
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
./scripts/test-installer.ps1
cargo run --locked -- --preview
./scripts/package.ps1
```

The embedded executable icon contains nine sizes from 16 to 256 pixels. `assets/logo.svg` is the editable mark. Regenerate the PNG, raw window icon and ICO with `./scripts/generate-icons.ps1`.

`python scripts/check-source-privacy.py --staged` scans staged content for private paths, IP addresses, saved credentials and common key/token patterns without printing matching values. It is a focused safeguard, not a comprehensive security audit. Local config, metadata, logs and generated files must stay ignored.

## Versioning

`Cargo.toml` is the version source. Use Semantic Versioning and record user-facing changes in `CHANGELOG.md`. For each release:

1. Update the package version and lockfile, then add release notes.
2. Run the format, lint, test, privacy and package checks.
3. Commit, tag the exact commit as `vMAJOR.MINOR.PATCH`, and push the branch and tag.
4. The Windows workflow validates the tag/version match and attaches the ZIP and SHA-256 checksum to a GitHub Release. Verify the hosted workflow before distributing it.

Releases inherit the repository visibility and access restrictions. Builds are unsigned.

## License

Licensed under the [MIT License](LICENSE).

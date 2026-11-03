# Changelog

All notable changes are recorded here. Versions follow Semantic Versioning.

## 0.3.0 — 2026-11-03

- Add independent Files and Veeam profiles with isolated settings, credentials, logs, statistics and workers; preserve the existing Veeam profile in place.
- Add recursive SSH folder monitoring, include/exclude glob rules, quiet periods, per-file scheduling, rules-only preset export, action preview and recent run history.
- Stage generic file updates, verify resumed block content before publishing, replace atomically and retain previous versions.
- Protect unmanaged and locally changed files by default; support keep, quarantine and explicit mirror deletion for managed files only.
- Persist ownership through a replayable journal; scope transfer caches to source identities and nanosecond file revisions.
- Reject overlapping destinations, links/junctions, unsafe Windows names, case collisions and incomplete source scans.
- Coordinate shutdown and application updates across every profile; add a bilingual Files preview and virtualize dashboard rows.

## 0.2.0 — 2026-11-03

- Add a self-contained root installer with a fixed per-user installation folder, stable shortcuts, an installation manifest and existing-startup migration.
- Add in-app update checks, verified background downloads and safe update/restart in Turkish and English.
- Support private releases through existing GitHub CLI authentication, token environment variables or an optional DPAPI-encrypted token.
- Validate checksums, asset digests, archive paths, executable versions and monotonic updates; preserve the previous EXE and restore it on installation failures.
- Wait for engine shutdown and final statistics before handing executable replacement to a separate process.
- Include the MIT license in distribution archives.
- Add installer boundary tests and cache release build artifacts in Windows CI.

## 0.1.0 — 2026-11-03

### Added

- Oh My Backup branding, editable logo and nine-resolution Windows executable/window icon.
- Desktop and Start menu installer with matching taskbar identity.
- Turkish and English UI, saved language preference and quick language selector.
- Responsive dashboard, horizontally scrollable backup table and contextual backup details.
- Offline sample-data preview for UI review without connecting to a server.
- Safe exit prompt while transfers are active.
- Source privacy guard, Windows CI and versioned ZIP/checksum release workflow.

### Fixed

- Missing or truncated local backups no longer appear current solely because their history matches the server.
- Invalid scheduler settings are rejected before the sync engine starts filesystem or network work.
- Corrupted block sidecars return an error instead of overflowing their count calculation.
- One-shot and desktop sync now share single-instance protection.
- Failed immediate setting saves no longer change the in-memory setting.
- Legacy cleanup approval explains possible deletion before download when space is low.

### Privacy

- Removed installation-specific server addresses, key fingerprints and customer names from source defaults and fixtures.
- Excluded local configuration, backup metadata, logs, credentials and development scratch files from version control.

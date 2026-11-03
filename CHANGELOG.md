# Changelog

All notable changes are recorded here. Versions follow Semantic Versioning.

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

# Desktop review and validation

## Round 1 — desktop, configuration and local-state correctness

### Summary

Three original defects were reproduced with failing tests, corrected and retained as permanent regressions. The desktop work also adds branding, bilingual UI, packaging and safer interaction feedback. This is not a full security audit or a live restore certification.

### Toolchain

Rust 1.93.0 (pinned in `rust-toolchain.toml`), Cargo, the built-in Rust test runner, rustfmt and Clippy. `Cargo.toml` defines dependencies and the release profile. The Windows SDK resource compiler embeds icon and version resources through `build.rs`. No race detector or sanitizer is configured.

### Findings

#### F01 — bug — High — Missing local backups reported current

- Location: `src/sync.rs`, `refresh_views` and `pick`.
- Root cause: matching server records and committed history were treated as sufficient, without checking the local files.
- Proof before: `deleted_or_truncated_local_backup_is_pending` passed its intact-file control, then printed `EXPECTED: missing backup is not current; ACTUAL: in_sync=true` and failed.
- Patch: current status requires a complete set with each expected local filename and size. The scheduler no longer skips damaged local sets. Transfer estimates include missing/truncated files.
- Proof after: the same test checks missing and truncated files, pending scheduling and nonzero repair estimates; it passes.
- Regression test: `src/sync.rs`, `deleted_or_truncated_local_backup_is_pending`.
- Limit: this detects missing/truncated files, not same-size bit corruption.

#### F02 — input-handling — Medium — Invalid scan interval accepted

- Location: `src/config.rs`, configuration validation and engine startup.
- Root cause: UI validation allowed a zero scan interval, which could reach a reversed clamp range in the engine. Manually edited JSON bypassed the UI entirely.
- Proof before: `settings_reject_invalid_scheduler_values` accepted normal defaults with a sample hostname, then printed `EXPECTED: invalid scan interval rejected; ACTUAL: Ok(())` and failed.
- Patch: shared configuration validation rejects invalid scan intervals, hours, margins, speed and disk limits before engine filesystem/network work.
- Proof after: the same regression rejects zero intervals, nonfinite speed and out-of-range hours.
- Regression test: `src/ui.rs`, `settings_reject_invalid_scheduler_values`.

#### F03 — input-handling — Medium — Corrupt block count panicked

- Location: `src/blk.rs`, `Blk::load`.
- Root cause: the untrusted serialized block count was multiplied before validating the payload size, allowing integer overflow.
- Proof before: a valid sidecar loaded successfully; replacing its count with the maximum integer caused a panic, caught and reported as `EXPECTED: Err without panic; ACTUAL: panicked=true`.
- Patch: validate count against the actual payload length before further use.
- Proof after: the original case returns an error without panic; empty, truncated and invalid-header controls also return errors.
- Regression test: `src/blk.rs`, `corrupt_block_count_returns_error_instead_of_panicking`.

### Additional behavior checks

- A bilingual log-filter regression was observed failing on an English connection error before correction.
- The single-instance regression uses an isolated named mutex, so it can run while a production copy is transferring.
- Translation tests check placeholder order, legacy config defaults, saved language round trips and switching existing view messages without changing filenames.
- The UI render test draws the complete offline preview, settings and details at 900×500 and 1500×900 in both languages, with explicit dark theme assertions.
- Four Python privacy-guard tests cover clean source, an empty index, forbidden config files and redacted token/address detection.
- The installer was exercised in isolated folders; shortcut creation and matching AppUserModelID completed successfully.

### Validation

- `cargo fmt --check`: passed.
- `cargo clippy --locked --all-targets -- -D warnings`: passed.
- `cargo test --locked`: 13 passed, zero failures or ignored tests.
- `python -m unittest discover -s scripts -p 'test_*.py'`: four passed.
- `python scripts/check-source-privacy.py --staged`: source patterns checked with matching values redacted on failures.
- `scripts/package.ps1`: optimized Windows x64 build and versioned ZIP/checksum produced.
- `python scripts/verify-package.py`: verifies checksum, archive allowlist, PE architecture, nine embedded icon resources, absence of private build paths and extracted-binary `--version` startup.
- `git diff --cached --check`: passed.
- The original three regression proofs were run repeatedly after the patch. Local proof bodies and a ledger are retained in ignored `.temp_files` and are excluded from GitHub.

### Not reproduced (dropped)

None of the three original candidates were dropped.

### Boundaries

An early native preview was opened and inspected. Subsequent Computer Use was stopped through Escape; no further UI automation was performed. The final bilingual layout is covered by actual egui rendering tests, while a final interactive visual review and a complete active-transfer exit test remain unverified. No live backup transfer, destructive cleanup or restore test was started for this review. An already-running production transfer was left running.

### Working tree

The repository originally had no commits and its project files were untracked. Existing local assistant/config/metadata folders were preserved and ignored. A stale Git index lock was moved to an ignored backup after checking for active Git processes. Only project source, assets, documentation, checks and release tooling are versioned.

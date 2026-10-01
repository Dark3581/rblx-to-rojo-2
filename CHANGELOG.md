# Changelog
All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.2.0] - 2026-09-30
First release of the maintained fork at https://github.com/Dark3581/rblx-to-rojo-2.

### Fixed
- Siblings with the same name (e.g. four models named `Light`, each with a script) were merged into one folder and their scripts overwrote each other without a warning. The parent is now saved as a `.rbxm` model with every copy intact. If the parent is a service, which can't be bundled, the duplicates are saved to `src/__conflicts__/` and a warning is logged.
- Names that can't be file names (`AC/Ban`, `init`, `CON`, names ending in `.` or in a Rojo suffix like `.server`) crashed the tool or put scripts in the wrong place. Service children with such names are now written under `src/__named__/` and mapped back to their real names in `default.project.json`. Deeper ones are bundled with their parent.
- Disabled scripts, `RunContext`, attributes and tags were dropped. They're now written to `.meta.json` files.
- The log file only started after the output folder was chosen, so it missed the first lines of every run (including decoder warnings). It now has the whole run, with levels.
- Every place saved by current Studio printed `WARN rbx_binary … Unknown value type ID 0x23 … Terrain.VoxelGridAssetContentMap`, which looked like a failure. Unsupported property types that the export doesn't use are now logged as one plain INFO line saying nothing was lost. They're still a WARN if they hit a property the export reads.
- File write errors crashed the tool. They're now reported and the export carries on.
- A place with no scripts crashed when writing `default.project.json`.

### Added
- A summary in the log of where every script went: written as files, inside `.rbxm` bundles, saved to `__conflicts__`, or in ignored services.
- Warnings for scripts inside services that aren't exported, and for classes missing from the reflection database.
- A warning when the output folder already has files from an earlier run.
- A `--luau` flag to write `.luau` files, and `--help`.
- `RUST_LOG` is respected.
- When started without arguments (double-clicked), the window stays open until Enter is pressed.
- Tests for duplicate names, bad names and script properties. Tests now also check that every script is accounted for.
- Weekly scheduled CI on Windows and Linux, including a run against the newest compatible dependencies.

### Changed
- Upgraded `rbx_binary` (3.0), `rbx_xml` (3.0), `rbx_dom_weak` (4.2), `rbx_reflection` (7.0), and `rbx_reflection_database` (3.0.0+roblox-728) so places saved by current Studio builds decode again. The old database surfaced as `Type mismatch: Property Animator.Tags should be SharedString, but it was Tags`.
- Updated GitHub Actions to current versions. `upload-artifact@v1` no longer works.

## [1.1.1] - 2026-06-11
### Changed
- Replaced `nfd-rs` with **`rfd`** for native file/folder dialogs, removing the `future-incompat` warning from the old `nfd` dependency.

## [1.1.0] - 2026-06-11
### Changed
- Upgraded `rbx_binary`, `rbx_xml`, and `rbx_dom_weak` to current crates.io releases so modern `.rbxl` / `.rbxm` files (including ZSTD-compressed chunks) decode correctly. Roblox’s binary format outgrew the old bundled decoder, which surfaced as `Decompression failed. Input invalid or too long?`.
- Reflection lookups now use the bundled `rbx_reflection_database` with `ClassTag::Service` instead of the removed `get_class_descriptor` / `is_service` API.

## [1.0.1] - 2021-04-11
### Fixed
- Fixed newer builds not being usable.

## [1.0.0] - 2021-01-06
### Added
- Added support for .rbxl and .rbxm, and not just .rbxlx.

### Changed
- Changed file reading mechanism to be one that should be more optimized, increasing read times. You can further increase read times by switching to binary (.rbxl, .rbxm) files instead of using .rbxlx.

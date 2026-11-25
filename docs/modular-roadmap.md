# Modular project roadmap

This is the fork's planning record for #1049. The first desktop milestone is single-editor Open → Edit → Save → Recover → Export on a local disk. A standard project remains the default exchange format. Each row below can be reviewed upstream independently, after its prerequisites. Continue on the fork if upstream declines a slice.

| Capability and implementation | Evidence on the integration branch | Next acceptance criterion and review boundary |
| --- | --- | --- |
| Record boundaries in craft-fcproj-format, conversion in filmcraft-format | The converter suite round-trips bin order, stable IDs, unknown fields, and offline original/proxy paths. | Review the shared record API separately from the FilmCraft converter and dependency pin. |
| Atomic incremental save and historical rescue in project_folder | Publication-boundary, damaged-record, bin and sequence rescue tests exercise immutable payloads, internal edit stamps, and independently readable exports. | Submit revision persistence and historical rescue separately. Cross-volume backup still needs a restore with the original drive disconnected. |
| Folder editing, Undo and private checkpoints in filmcraft-engine | The folder engine and CLI suites cover saved deletions, restored extensions, failed switches, and killed writers. | Qualify the mixed-format engine lifecycle below before wiring desktop entry points. |
| Single-editor desktop integration | Native startup supplies autosave preferences, while the existing folder CLI has its own journal. | First prove standard → folder → standard, recovery from both stores and busy-reader export in one desktop-configured engine. Then review desktop Open, Save, Recover and Export with keyboard and automation coverage. |
| Bin checkout on one local disk in shared_bins | Separate CLI processes save in both orders, reject competing ownership, and recover a draft over a peer save. | Review independently of desktop editing. Qualify local Windows and Linux filesystems alongside macOS. |
| Multi-machine shared storage | Local process locks provide a baseline. | Before offering network writes, test two machines on each supported filesystem through disconnect, handoff, stale reconnect and interrupted publication. Publish the supported configurations. |
| Concurrent edits inside one sequence | The prototype has a shared-timeline experiment. Integration leases the whole sequence. | Decide ownership for structural versus item edits. Require independent edits to survive and overlapping trims, linked clips, transitions and Undo to be rejected or reconciled deterministically. |
| Reference search, pinned checkpoints, linked bins and on-demand loading | Historical subset rescue already traverses dependencies. Normal opens still load the whole project. | Separate proposals for a closed-bin reference inspector, backup/retention, pinned read-only links, and bounded loading. Measure open time and memory on the same large fixture before choosing an index. |

## Prototype audit

The prototype on `codex/modular-project-storage` has local changes beyond its roadmap commit. The following is a source audit of those changes. Its tests describe behavior to reproduce on integration.

- **Bin exchange.** The bin-exchange panel calls `file.importProjectBin` to preview a dependency subset, including offline sources. Apply supplies the preview manifest. The engine reloads the subset, checks that revision, rebases paths and remaps IDs into the target in one Undo step. The prototype test `import_previews_then_remaps_references_and_undo_removes_only_the_import` covers that path. Integration already rescues a historical subtree with dependencies to a standard file. Port preview, stale-preview rejection and single-step import over that traversal. Before implementation, decide when importing twice should reuse an existing source versus create an independent identity.
- **Side-by-side version comparison.** The versions panel calls `project.compareModuleVersions`; the engine reads a saved subset and compares it with another revision or the open project. It reports member or clip changes and changed sources. The prototype's deleted-cut UI test and `search_deleted_by_old_path_then_compare_and_recover_without_changing_live_items` cover that workflow. Integration already lists revisions and rescues old cuts and bins. Port a read-only comparison first. Review replacement of live edits, changed dependencies, and cross-bin moves separately from comparison and restore-as-copy.

## Delivery

Keep tested changes in local commits on modular-integration. Upstream submissions should name their exact revision, tests and limits. The prototype's alternate storage layout and UI are reference material. See the [project-folder guide](project-files.md#developer-project-folder-converter) for the commands available on this branch.

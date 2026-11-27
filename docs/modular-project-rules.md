# Modular project rules audit

This audit covers the local schema-19 folder editor for #1049. **Cuts** and **Selects** are bins. **Interview** is an editable sequence used as a source inside another sequence, **Master**. A bin entry locates Interview in the project panel. A timeline clip refers to Interview by its project-wide item ID. A master can therefore use a source that has no bin entry.

**Verified** means a check exercised the stated behavior. **Implementation gap** identifies an observed mismatch or a missing operation, followed by the test that should pass when it is fixed. **Decision needed** identifies a product choice. A check of a prepared project establishes what the reader can do with that project. Creating the same state through Save, Discard, or crash recovery requires its own check.

## Saving and leaving the editor

| Rule | Status | Evidence or acceptance test |
| --- | --- | --- |
| Menu Save and the Save shortcut publish all outstanding bin edits and changed project settings together, wherever the editor is working. | Verified | In the headless desktop UI, edit Cuts, Selects, and the title-safe settings. Exercise both File → Save and Cmd+S with an open bin in the Project panel, then repeat from the Timeline panel. All four cases reopened with both bin edits and the settings. The third bin and all untouched item records kept their digests. Both entry points dispatch the shared Save command. |
| A settings-only save updates the project envelope and revision metadata. A clean Save writes zero bytes and preserves the revision. | Verified | Compare the manifests, byte count, and reopened settings in the engine folder tests. A stale editor or a damaged live record produces an error, keeps the edit Unsaved, and preserves the published manifest. |
| A background checkpoint leaves the folder at its last explicit save. | Verified | With the preference to save the current project enabled, make an edit and trigger backup. The manifest remained byte-identical, the session stayed dirty, and a fresh session recovered the draft separately. The desktop indicators follow edits against the last published folder revision. |
| Closing a dirty project or requesting app quit preserves edits until the project prompt is answered. | Verified | On a folder, test Save and Don't Save through both prompts, then reopen independently. Save published the edit and Don't Save preserved the previous manifest. A stale save kept the prompt and local edits and preserved the other writer's revision. The two-choice **bin** prompt below has a different scope. |
| A confirmed app quit lets the native window close. | Implementation gap | The existing headless window-close test passed the initial prompt, Cancel, and Don't Save assertions. It failed on the final viewport close, which still reported CancelClose. It failed again in isolation. Reproduce with `clip_dialogs_ui::quitting_with_unsaved_changes_asks_to_save`. Acceptance requires a confirmed quit to let the window close. A native desktop reproduction is outstanding. |

The File menu and keyboard route to `file.save`. The folder writer compares the loaded project and view to the saved baseline and publishes one manifest. A successful global Save advances the session's saved revision. The project close and quit handlers consult session dirtiness and invoke global Save. Bin close dispatches the selective Save or Discard command. [Folder writer](../crates/engine/src/folder_edit.rs) · [Save and recovery commands](../crates/engine/src/commands.rs) · [Project and window prompts](../crates/ui-egui/src/panels/clip_dialogs.rs) · [Folder command tests](../crates/engine/src/file_tests.rs)

## Bin views, close, and moves

| Rule | Status | Reproduction and acceptance test |
| --- | --- | --- |
| Each bin has one open view per session. Reopening brings it forward and relocating it preserves its edits. | Verified for local folders | Navigate from edited Cuts to Selects, reopen Cuts, move it to a floating window, then restore a reordered layout with a duplicate. Each bin retains one view and its own edits. A pending close still targets its original bin. |
| Closing a bin closes its open sequences. A dirty bin asks once for Save or Discard. Failure leaves its views, timelines, and edits open. | Verified for independent bin edits | Save Selects by keyboard, then reopen the folder independently. Its timeline closes and the master in Cuts still renders Interview. Closing a clean parent leaves its child timeline open. A stale revision or blocked write keeps the view and timelines open. Retrying a published save completes cleanup with the same manifest. |
| Dropping Interview onto a closed destination does nothing. | Verified for local folders | In list and icon views, drop onto a destination with no open view. Membership, undo history, dirtiness, and the manifest stay unchanged. Opening the destination in a background tab allows the move once. Expanding a list row alone leaves the destination closed. |
| A move uses the bins’ ordinary Unsaved state. Saving Bin B on close commits its arrival alone. | Verified for sequence moves | Bin A’s saved record keeps its digest. Saving Bin A later, individually or through global Save, removes its surviving entry. Discarding Bin B first leaves Bin A’s removal Unsaved. |
| The last saved arrival keeps the original sequence ID. Earlier surviving cuts have independent IDs. | Verified for local folders | Kill the editor after saving Bin B and after saving Bin C. On fresh opens, two masters follow the same picture, sound and Match Frame target before any bin opens and after all six opening orders. Renaming an older bin and checking out a standard project preserve the IDs and cuts. |
| Before Bin A can save its removal, the last explicit save and the cut as it left Bin A must be recoverable. | Verified for local folders | Corrupt the departure backup, verify the removal fails, repair the backup and retry. A journal cleanup failure after publication retries without advancing the revision. Restore the departure version as a copy alongside later edits. |
| Selective Save and Discard preserve the other bins’ unsaved recovery data. Startup respects an explicit Discard. | Verified for independent edits | Checkpoint both timeline edits and a changed setting. Kill the process after the UI acknowledges Discard. The detached draft recovers Cuts and the setting, and Selects matches the published cut. The engine also covers process interruption immediately after Save. |
| An explicit Discard is final for that bin. Independent Undo and Redo remain available. | Verified for independent engine operations | Interleave timeline edits in Cuts and Selects, undo twice, then discard Selects. Traverse both history stacks. Only Cuts changes, and returning to the published state clears Unsaved. |


The bin-close prompt captures the document and bin ID. Sequence moves are supported between writable local bins. Selective publication of moved media items and child bins retains its existing conflict checks. [View and close handling](../crates/ui-egui/src/panels/project/bin_views.rs) · [Transfer playback and crash tests](../crates/ui-egui/tests/it/modular_bin_ui/transfer_playback.rs) · [Engine decision tests](../crates/engine/src/folder_edit/tests.rs)

## Source recall, recovery, and retention

| Rule | Status | Evidence or acceptance test |
| --- | --- | --- |
| A prepared project may keep a shared source outside all bins. Masters can play, export, and recall it. | Verified | In a schema-19 fixture, two masters use Interview, which contains a two-angle edit. A second fixture gives the winning cut and two older cuts distinct IDs across three bins. On fresh opens, both masters matched the saved reference picture, PCM audio, PNG frame export, WAV export and Match Frame ID/time before opening bins and through all six orders. The alternate angle had distinguishable picture and sound. An unrelated save and Restore as Copy preserved both links. |
| Saving Bin A’s removal and discarding Bin B leaves Sequence 1 outside all bins. Masters keep the last explicitly saved version. | Verified before pruning | Two masters reopen, play, export picture and sound, and Match Frame to Sequence 1 with its camera angles. Version 2, captured at departure, restores independently. Version 3, discarded in Bin B, stays discarded after a forced quit. Repeat after retention is implemented. |
| Restore as Copy preserves today's edits. Changed required sources can be recovered in a separate rescue export. | Verified | The converter restored a historical cut under a fresh ID while preserving current edits and view. A required source with changed RAW settings prevented an in-project restore. Rescue exported the historical context separately. Corrupt required history was rejected. Schema-19 RAW settings also survived convert → reopen → standard save → reopen. |
| Prune folder recovery versions after a count of successful published saves, with per-bin and project limits. | Implementation gap | The existing rotation limits dated whole-project backups. Folder history has no count-triggered collector. Advance the count with real changes, interleave no-op and failed saves, preview eligible versions, then prune. Reopen the live project and recover from protected history. Protect referenced sources and their dependencies, active recovery, explicitly kept versions, and the backup promised by a move. |
| Select the pruning interval and retention limits. | Decision needed | Choose these during retention design using measured growth and a usable restore window. No-op saves and failures do not advance the count. |
| Drag an item from another editor's locked bin into a writable bin. | Decision needed | Decide whether to create an independent duplicate in the destination or reject the drag. The source must stay intact. The existing write lock serializes folder publication and does not establish per-bin ownership. |

The prepared two-camera control and the live transfer tests use generated media. The transfer tests compare rendered frames, PCM samples, PNG and WAV exports, and Match Frame. Post-prune reopen remains part of the retention stage. [Source lookup and moves](../crates/project/src/lib.rs) · [Historical recovery](../crates/format/src/project_folder/recovery.rs)

## Avid comparisons

- **Save and backups.** Media Composer documents active-bin and all-bin saves, automatic saves, and limits on backup copies. FilmCraft chose global Save from any focus and checkpoints that keep the bin Unsaved. FilmCraft's closed-bin drop rule also differs from Avid's documented drag-to-open behavior. [Media Composer Editing Guide, Saving Bins](https://resources.avid.com/SupportFiles/attach/Media_Composer/Media_Composer_v2025.x_Editing_Guide.pdf#page=298) · [Bin Settings](https://resources.avid.com/SupportFiles/attach/Media_Composer/Media_Composer_v2025.x_Editing_Guide.pdf#page=1152)
- **Interrupted move.** In the editor's test, they moved a sequence from Bin1 to Bin2, saved Bin2 alone, and force-quit. The entry in Bin1 appeared as `AutoCopy.01` on reopen. The suffix establishes the visible result. Open order and sequence Mob IDs were unverified. Avid describes duplicating and renaming a sequence in the second bin opened in its Production Management chapter. FilmCraft chose the last saved move destination as the authority for existing references. [Avid Editing Guide, p. 1049](https://resources.avid.com/SupportFiles/attach/Media_Composer/Media_Composer_v2025.x_Editing_Guide.pdf#page=1049)
- **Source recall and locks.** Avid documents matching from a multicamera edit to its group and then its source. This does not establish what Avid does after its last bin entry disappears. Avid separately documents duplicating an item dragged out of a locked bin and disallowing a drop into a locked bin. FilmCraft's shared-editing choice remains open. [Multicamera Match Frame](https://resources.avid.com/SupportFiles/attach/Media_Composer/Media_Composer_v2025.x_Editing_Guide.pdf#page=1134) · [Bin locking](https://resources.avid.com/SupportFiles/attach/Media_Composer/Media_Composer_v2025.x_Editing_Guide.pdf#page=304)

## Performance evidence

The global Save recheck for the single-bin engine stage used the same already-loaded edit and local APFS fixtures. Times are medians of five measured trials. Bytes are application writes. Peak RSS is the whole-process high-water mark for the open and save runs.

| Timeline clips | Format | Open | One-reel Save | Bytes written | Peak RSS, open / save |
| ---: | --- | ---: | ---: | ---: | ---: |
| 24,000 | Standard .fcproj | 115.6 ms | 214.6 ms | 18,042,189 | 119 / 155 MiB |
| 24,000 | Project folder | 321.0 ms | 152.2 ms | 1,597,340 | 529 / 547 MiB |
| 48,000 | Standard .fcproj | 229.1 ms | 382.5 ms | 36,104,282 | 230 / 302 MiB |
| 48,000 | Project folder | 645.2 ms | 231.6 ms | 3,190,940 | 1,046 / 1,088 MiB |

The folder passed the one-reel save-time gate at both sizes. Open time and peak memory remain higher. The earlier storage runs additionally exercised process interruption around manifest publication and injected StorageFull failures. Actual volume exhaustion, shared-filesystem durability, and physical read/write amplification remain unmeasured.

## Sequence-transfer performance

Each size ran one warm-up and five measured trials on the same local Mac. Times are medians. Peak resident memory is the largest result across the measured trials. The arrival saves Bin B alone in the folder project, and the standard project saves the complete move.

| Clips | Format | Open (ms) | One-bin trim (ms) | Save arrival (ms) | Arrival bytes | Peak RSS, open / arrival (MiB) |
|---:|---|---:|---:|---:|---:|---:|
| 24,000 | Standard .fcproj | 116.1 | 225.9 | 203.6 | 18,042,189 | 119 / 140 |
| 24,000 | Modular folder | 325.2 | 146.1 | 109.6 | 188,423 | 529 / 535 |
| 48,000 | Standard .fcproj | 231.2 | 399.6 | 356.6 | 36,104,282 | 230 / 269 |
| 48,000 | Modular folder | 643.7 | 193.4 | 185.1 | 372,027 | 1048 / 1058 |

Both sizes passed the one-bin trim gate. Saving an arrival reused the prior sequence payload and kept Bin A’s saved bin digest. A clean folder Save wrote zero bytes. Folder open remained slower and used more memory at both sizes.

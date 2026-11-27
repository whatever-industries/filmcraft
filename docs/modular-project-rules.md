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
| Saving Bin A’s removal and discarding Bin B leaves Sequence 1 outside all bins. Masters keep the last explicitly saved version. | Verified in local transfer tests | Two masters reopen, play, export picture and sound, and Match Frame to Sequence 1 with its camera angles. Version 2, captured at departure, restores independently. Version 3, discarded in Bin B, stays discarded after a forced quit. |
| Restore as Copy preserves today's edits. Changed required sources can be recovered in a separate rescue export. | Verified | In the editor, Restore as Copy adds an Unsaved sequence with a fresh ID and preserves the current cut, settings, and open timelines. Undo removes the copy. Save and reopen keep both cuts. The separate project export retains the historical nested source when its working version changes. A selected history entry that has since been pruned cannot restore over current work. A required source with changed RAW settings prevented an in-project restore. Rescue exported the historical context separately. Corrupt required history was rejected. Schema-19 RAW settings also survived convert → reopen → standard save → reopen. |
| Count each bin whose edits are published. Settings-only, clean and failed saves add zero. | Local implementation | Closing and saving one bin adds one. A global Save of two edited bins adds two. The count survives reopening. |
| Automatically prune above the configured bin-backup count. | Local implementation | Defaults to 2,000 per project. The project’s Recovery settings expose the switch and limits. Preview lists eligible versions. The editor schedules cleanup in the background. A changed head, pin or policy invalidates stale work. Reopening resumes interrupted collection. Tests protect current and kept versions, move backups and their source records. |
| Drag an item from another editor's locked bin into a writable bin. | Decision needed | Decide whether to create an independent duplicate in the destination or reject the drag. The source must stay intact. The existing write lock serializes folder publication and does not establish per-bin ownership. |

The prepared two-camera control and the live transfer tests use generated media. The transfer tests compare rendered frames, PCM samples, PNG and WAV exports, and Match Frame. The transfer and force-quit cases also run with a one-backup limit. [Source lookup and moves](../crates/project/src/lib.rs) · [Historical recovery](../crates/format/src/project_folder/recovery.rs)

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

The one-reel writes above put 5 GB in the range of roughly 1,600 to 3,100 saves in those two fixtures. This informed the starting count of 2,000. Writes include manifest publication, and shared records are stored once. Actual recovery storage varies with bin contents and edit type. The editor uses the published bin count alone to trigger cleanup.

### Save during recovery cleanup

Three runs per size each changed the same reel’s start timecode five times on local fixture copies. A two-backup limit forced cleanup on saves three through five. After each run, the editor reopened the live cut at frame 5 and the restored copy at frame 3. A legacy revision was already present in each fixture. Times are median elapsed milliseconds for editor Save on this machine.

| Timeline clips | Standard .fcproj | Modular Save before cleanup | Modular Save with cleanup |
| ---: | ---: | ---: | ---: |
| 24,000 | 217 | 97.5 | 184 |
| 48,000 | 387 | 178 | 275 |

A separate small-project run used the default settings. It retained all 2,000 backups at the threshold. Save 2,001 reduced that to 1,800 backups plus the live version. The retired cut’s unique record was removed, the recent cut’s record remained, and the live project reopened. Cleanup ran under the project writer lock. The feature-scale timings cover a short history. A separate 24,000-clip run subsequently exercised the default threshold. Revision manifests remain in the project for history and move lookup.

On a local APFS volume, a 24,000-clip project received 2,001 explicit one-reel saves with background journaling inactive. The project folder grew from 18.2 MB initially to 3.03 GB at save 2,000. That save took 133 ms. Save 2,001 took **17.65 seconds** with cleanup under the writer lock and left 2.75 GB. Reopening preserved cut 2,001 and restored cut 1,999 as an independent copy. This was the synchronous baseline.

The background implementation was exercised on the same local 24,000-clip fixture. A fresh run of 2,001 saves verified cut 2,001 and restored cut 1,999 independently. A second run started from a verified copy of the 2,000-backup history and made another edit during cleanup. The following single-run timings include normal editor publication. Background completion is wall time, including concurrent editing in the final row.

| Editor run | Save 2,001 | Save 2,002 | Cleanup completion | Reopen and restore |
| --- | ---: | ---: | --- | --- |
| Synchronous baseline | 17.65 s | — | Before Save returned | Current cut 2,001 and copy of 1,999 |
| Background, fresh 2,001-save run | 326 ms | — | 25.1 s after Save returned | Current cut 2,001 and copy of 1,999 |
| Background, another Save during the history scan | 204 ms | 80 ms | 27.5 s from the first edit | Current cut 2,002 and copy of 2,000 |

The overlap run retained 1,800 ordinary bin backups and finished with zero pending deletions. The worker checks for changes during long scans. It verifies that the revision and retention catalog are still current under the writer lock before publishing the catalog and before each batch of up to 16 unlinks. A regression test reuses a retired sequence record in a newer save between catalog publication and deletion, then reopens the live cut after cleanup. These measurements cover a local APFS volume.

### Browse and restore from the editor

A local editor run used a copy of the feature-scale retention fixture with over 2,000 saves. Listing the first page took 20 ms. Listing the sequences in save 1,999 took 222 ms, and restoring one of them into the working project took 144 ms. After Save and reopen, the copy retained start timecode 1,998 and the working sequence retained its edit at 2,012. These are single-run measurements for a recent version on local APFS. The keyboard and accessibility-tree test separately covers Undo, Redo, a changed nested source, a separate export, and refusal to overwrite an existing export.

### Incremental crash checkpoints

Each comparison opened the same modular fixture and passed its working model to the native recovery worker. Three fresh sessions per writer each made six linked one-reel trims. Elapsed time and bytes are medians. The full-snapshot byte counter measures the project payload. The modular counter includes the changed records, index and publication marker. Peak RSS is the largest whole-process resident set sampled during a checkpoint. The first checkpoint includes the full working project. On each subsequent trim the modular writer wrote one timeline record, the index and the marker.

| Timeline clips | Recovery writer | First time | Repeat time | First / repeat bytes | First / repeat peak RSS |
| ---: | --- | ---: | ---: | ---: | ---: |
| 24,000 | Full snapshot | 261 ms | 238 ms | 18.05 / 18.05 MB | 564 / 658 MiB |
| 24,000 | Modular checkpoint | 434 ms | 82 ms | 18.15 / 1.52 MB | 564 / 574 MiB |
| 48,000 | Full snapshot | 404 ms | 395 ms | 36.11 / 36.11 MB | 1,118 / 1,310 MiB |
| 48,000 | Modular checkpoint | 531 ms | 104 ms | 36.32 / 3.04 MB | 1,119 / 1,137 MiB |

The modular writer flushes changed records together and recycles retired byte ranges after publication. Forced process termination on either side of marker publication recovers the corresponding complete draft. The editor tests cover Save Cuts, Discard Selects, an outstanding project setting, original-folder relocation, and a failed handoff to a new recovery session. A storage-full retry with a newer edit and repeated trims checks reclamation. Physical power loss and network filesystems remain unqualified.

## Sequence-transfer performance

Each size ran one warm-up and five measured trials on the same local Mac. Times are medians. Peak resident memory is the largest result across the measured trials. The arrival saves Bin B alone in the folder project, and the standard project saves the complete move.

| Clips | Format | Open (ms) | One-bin trim (ms) | Save arrival (ms) | Arrival bytes | Peak RSS, open / arrival (MiB) |
|---:|---|---:|---:|---:|---:|---:|
| 24,000 | Standard .fcproj | 116.1 | 225.9 | 203.6 | 18,042,189 | 119 / 140 |
| 24,000 | Modular folder | 325.2 | 146.1 | 109.6 | 188,423 | 529 / 535 |
| 48,000 | Standard .fcproj | 231.2 | 399.6 | 356.6 | 36,104,282 | 230 / 269 |
| 48,000 | Modular folder | 643.7 | 193.4 | 185.1 | 372,027 | 1048 / 1058 |

Both sizes passed the one-bin trim gate. Saving an arrival reused the prior sequence payload and kept Bin A’s saved bin digest. A clean folder Save wrote zero bytes. Folder open remained slower and used more memory at both sizes.

## Project open and first bin activation

The writable editor reads the entire modular project on open. On a 48,000-clip project that took 574 ms, compared with 164 ms for the equivalent standard project. To measure what could be deferred, a read-only experiment opened the same modular folder, read the complete bin directory and global settings, and followed item references from the active timeline across closed bins. It also measured the cost of loading every saved timeline tab. Both fixtures had 12 open reel tabs and 24 shoot-day bins.

| Timeline clips | Open operation | Median open (ms) | Project data at open (MB) | First shoot-day bin (ms) | Peak RSS (MiB) |
| ---: | --- | ---: | ---: | ---: | ---: |
| 24,000 | Standard editor | 82 | 18.13 | 0.010 | 126 |
| 24,000 | Modular editor | 283 | 18.23 | 0.042 | 536 |
| 24,000 | Reader, active reel | 33 | 2.44 | 0.190 | 21 |
| 24,000 | Reader, all saved tabs | 129 | 18.23 | 0.012 | 106 |
| 48,000 | Standard editor | 164 | 36.31 | 0.015 | 236 |
| 48,000 | Modular editor | 574 | 36.49 | 0.045 | 1,053 |
| 48,000 | Reader, active reel | 66 | 4.89 | 0.396 | 32 |
| 48,000 | Reader, all saved tabs | 261 | 36.49 | 0.027 | 202 |

One warm-up preceded five fresh-process trials per case, with rotating case order on local APFS, using the workspace dev profile. Editor timings cover the `file.open` command, then bin navigation through construction of the visible item list. Reader timings cover reading, hashing and decoding the selected project records. Editor media registration, relink checks, full validation and GUI rendering are outside the reader measurement. MB denotes logical project bytes: file size for the standard project, size of the live manifest and its records for the fully loaded folder, and bytes actually read by the experimental reader. RSS is the largest whole-process high-water mark through first activation, from macOS `/usr/bin/time -l`.

The active reel required 1,000 of 1,200 sources at the smaller size and 2,000 of 2,400 at the larger size. Opening the first shoot-day bin then read only eight additional items (7,832 bytes) or 16 (15,876 bytes). Opening **Reels** first gave a different result. Loading all 12 timelines in that bin and their sources added 15.79 MB in 97 ms or 31.60 MB in 195 ms. A repeat activation read zero additional record bytes. Restoring every saved tab at startup also pulled in the entire project at both sizes.

The feature fixtures use offline media. A separate generated-media test places the master, nested cut and camera in three different bins. Before any bin navigation, the active-only reader's records produced the same sampled RGBA frame and non-silent PCM as the full editor. An unrelated missing record allowed active-only startup and produced an error when its bin was requested.

The next experiment separates showing a bin’s names from loading its timelines for playback. The measurements below cover that read-only path.

To reproduce on a project with a saved active sequence, build the native example and run the following commands. Choose a new destination directory and set `bin_id` to a bin in the source project. Use a footage bin and a bin containing several sequences for the two activation cases.

```sh
cargo build -p filmcraft-ui-egui --example bench_bin_open
bench="${CARGO_TARGET_DIR:-target}/debug/examples/bench_bin_open"
"$bench" prepare "$source_fcproj" "$destination"
"$bench" measure "$destination/project.fcproj" "$bin_id"
"$bench" measure "$destination/folder/project.fcmod" "$bin_id"
"$bench" active "$destination/folder/project.fcmod" "$bin_id"
"$bench" all-tabs "$destination/folder/project.fcmod" "$bin_id"
"$bench" verify "$destination/folder/project.fcmod" "$bin_id"
cargo test -p filmcraft-ui-egui --example bench_bin_open
```

The experiment supports fresh current-schema packs. Existing histories, reconciled move copies and project migrations are exercised through the production reader. [Benchmark and playback comparison](../crates/ui-egui/examples/bench_bin_open/native.rs) · [Generated-media regression](../crates/ui-egui/examples/bench_bin_open/tests.rs)

### Read-only preview and deferred timeline tabs

The local preview restores the saved tabs in order and renders the active cut at its saved playhead. Browsing a bin uses its directory and a name index. Selecting a timeline reads that cut and its missing sources, including sources in closed bins. The index is a separate file prepared from a fresh pack and bound to the manifest’s SHA-256 digest. A stale index produces an error. The preview has no editing, Save or recovery commands.

| Clips | Open path | First frame (ms) | List 12 reels (ms) | Switch to reel 2 and draw (ms) | Peak RSS (MiB) |
| ---: | --- | ---: | ---: | ---: | ---: |
| 24,000 | Standard .fcproj | 102 | 6.1 | 13 | 169 |
| 24,000 | Modular, full open | 307 | 5.9 | 13 | 577 |
| 24,000 | Modular, deferred preview | 52 | 6.2 | 24 | 77 |
| 48,000 | Standard .fcproj | 184 | 5.8 | 13 | 279 |
| 48,000 | Modular, full open | 596 | 5.9 | 13 | 1,095 |
| 48,000 | Modular, deferred preview | 86 | 6.2 | 33 | 92 |

All three paths used the same read-only window and quarter-resolution program image. The first two opened the project through the editor’s ordinary `file.open` command. Each measurement ended after painting the window and reading back the completed GPU image in a 960 × 720 offscreen viewport. The GPU was initialized before the timer. Physical display presentation and launching the desktop application are outside these measurements. The figures are medians of five fresh processes after one warm-up, with rotating case order on local APFS. RSS is the highest process peak through the tab switch.

Listing Reels read zero additional project records at either size. Switching to reel 2 read 68 additional records at each size, totaling 1.48 MB and 2.90 MB respectively. The first-frame figures assume an existing index. Building the 55 kB and 110 kB indexes separately took 187 ms and 427 ms respectively, including process launch and durable publication. A full rebuild on each open would erase the measured startup benefit.

The generated-media regression opens the master while a source required only by another saved tab is unavailable. The bin still lists that cut. Selecting its tab by keyboard reports an error and keeps the master frame visible. Restoring the source and selecting the cut again reproduces the ordinary editor’s frame and non-silent audio at the cut’s saved playhead. Returning to the master reuses the resident records. [Preview implementation](../crates/ui-egui/examples/bench_bin_open/preview.rs) · [Playback and retry test](../crates/ui-egui/examples/bench_bin_open/tests.rs)

After preparing a fixture with the commands above, choose a new path outside the project folder for `index_file`. The interactive window accepts the same index used by the headless comparison.

```sh
manifest="$destination/folder/project.fcmod"
"$bench" index "$manifest" "$index_file"
"$bench" preview "$manifest" "$index_file"
"$bench" frames "$destination/project.fcproj" "$bin_id"
"$bench" frames "$manifest" "$bin_id"
"$bench" frames "$manifest" "$bin_id" "$index_file"
```

**Next stage.** Validate the name index across real save history. Start with rename, move, Discard and historical restore as a copy. Measure the cost of refreshing only changed entries and check that an interrupted refresh or stale index can be rebuilt from the published revision. Verify identical bin listings and master playback after moves in every reopen order. Keep the production editor on its complete model until global search, relink, Save and recovery preserve and account for deferred records.

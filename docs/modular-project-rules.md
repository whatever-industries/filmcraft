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

The initial experiment covered fresh current-schema packs. The published-history checks below extend coverage to reconciled move copies. Project migrations still use the full reader. [Benchmark and playback comparison](../crates/ui-egui/examples/bench_bin_open/native.rs) · [Generated-media regression](../crates/ui-egui/examples/bench_bin_open/tests.rs)

### Read-only preview and deferred timeline tabs

The local preview restores the saved tabs in order and renders the active cut at its saved playhead. Browsing a bin uses its directory and a name index. Selecting a timeline reads that cut and its missing sources, including sources in closed bins. The index is a separate file bound to the manifest’s SHA-256 digest. A stale index produces an error. The preview has no editing, Save or recovery commands.

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

### Refreshing the preview after a published save

The preview reads bin membership from the published manifest and its bin records. It uses the same copy-identity rules as the full editor. A saved move can leave two logical cuts pointing to one immutable timeline payload. The index therefore keys each label by project item ID, payload digest, and any copy-identity override. A refresh reads changed item records, drops vanished IDs, and writes the complete index to a new file. Its checksum covers the revision, keys, and labels. An older index without keys can be upgraded with a full inventory read.

| Clips | Published edit | Full rebuild (ms) | Refresh (ms) | Item records read, refresh / rebuild | Item data read, refresh / rebuild (MB) |
| ---: | --- | ---: | ---: | ---: | ---: |
| 24,000 | One linked trim | 156 | 35 | 1 / 1,212 | 1.42 / 18.11 |
| 24,000 | Save the move destination | 169 | 43 | 1 / 1,213 | 1.42 / 19.53 |
| 48,000 | One linked trim | 287 | 43 | 1 / 2,412 | 2.83 / 36.27 |
| 48,000 | Save the move destination | 332 | 68 | 1 / 2,413 | 2.83 / 39.10 |

Each fixture received a one-reel trim and Save, then a move of that reel and Save of its destination alone. Five fresh processes followed a warm-up per case on local APFS. The table gives median elapsed time through durable index publication. Both paths also read the directory. The refresh reads the previous index and writes about 144 kB or 288 kB. Every refreshed index was byte-identical to the full rebuild for that revision. These are separate preview maintenance timings. Editor Save does not run the refresh.

The small generated-media fixture also exercised the following decisions:

- Renaming an item and its bin appeared after publication. Discarding an unpublished rename kept the previous index valid.
- After two destination-only saves, all six orders of visiting the three bins matched a full reopen. Each surviving cut rendered its own version, and the master used the last saved arrival throughout.
- Saving the source removal and discarding the destination left the cut out of both bin listings. The master could still play it by ID. Restoring an older cut as a copy preserved the current cut’s edit.
- A missing changed payload stopped publication. An unchanged, unavailable payload allowed its cached label to be listed and produced an error when requested for playback. Corrupt index data was rejected.
- On the 48,000-clip fixture a forced process kill during index-file staging left the project manifest and old index intact and the new index unpublished. The killed process left an unreferenced staging file. Retrying against the published revision produced the exact full rebuild.

To refresh, supply the previous index and a new destination file outside the project folder. Opening a stale index reports the mismatch. Use `index` with a new destination to rebuild if the previous index is missing or damaged.

```sh
"$bench" refresh "$manifest" "$previous_index" "$new_index"
"$bench" preview "$manifest" "$new_index"
```

### Automatic index refresh in the developer preview

Open a modular project with `"$bench" preview "$manifest"`. The preview locates a private cache by the project folder’s canonical path under FilmCraft’s per-user data directory. On open it reuses a current index, refreshes changed entries from a valid older index, or rebuilds a missing or damaged index. The explicit index commands above remain available for comparisons.

The preview builds the bin listing and renders the first frame from the same captured manifest. It checks the published manifest again before handing the view to the window. If a newer save arrived, it retries with that revision, up to twice. Continued changes produce a reopen message. After opening, the preview stays on the displayed save until reopened.

A cache-only lock protects staging, publication, and cleanup. A second preview that encounters the lock reports that the cache is busy. Editor Save takes no cache lock. A completed index replaces the previous file atomically. On the next open, the lock holder removes abandoned staging files in that project's cache.

The regression tests paused a refresh after staging, renamed the master and switched its camera through an ordinary editor Save, and compared the first frame, listing, and index against a full reopen and rebuild. A second test killed a process holding the cache lock after staging. Another preview had respected its live staging file. Reopening preserved the project and previous publication through the interruption, removed the abandoned file, and rebuilt the exact index for the saved cut. Warm reuse, incremental refresh with an unavailable deferred cut, damaged-cache rebuild, and the retry limit are also covered. [Automatic refresh tests](../crates/ui-egui/examples/bench_bin_open/cache_tests.rs)

### Project-wide Find and relink discovery across unopened bins

The local developer harness accepts the editor’s Find parameters and the existing Link Media search parameters. Each modular command reads one saved revision. Find returns ordered pairs of item ID and containing bin ID. An unfiled item has a null bin ID. Relink discovery returns the offline inventory and the editor’s ranked candidates, including failed checks and items with no candidate.

```sh
"$bench" search "$manifest" '{"column":"Name","operator":"contains","text":"Shot 000"}'
"$bench" search "$manifest" '{"column":"File Path","text":"shot-00017"}'
"$bench" relink-discovery "$manifest" '{"folder":"/path/to/moved/media"}'
```

The same commands accept a standard `.fcproj` for comparison. A name-only query uses the checksummed label index. The index records which items are hidden graphics and which are media. Older caches rebuild when that information is missing. Other Find queries read the searchable records and use the normal editor matcher, including parent media for subclip properties. Relink reads media records, registers their stored paths against the project document, and uses the editor’s media scan and candidate checks. An unavailable required record stops the operation with an error.

The small regression fixture exercised duplicate names in different bins, hidden graphics, metadata, and subclip video properties. A destination-only saved move, a discarded rename, and a source retained by the master after its last bin entry disappeared all matched the full editor’s Find order and bin membership. Moving an actual media folder produced the same missing items, candidate order, and diagnostics as a full reopen. The original image passed the fingerprint check, a same-named replacement failed it, and the other item had no candidate. Discovery preserved the saved manifest, media paths, and bin edit states. [Discovery tests](../crates/ui-egui/examples/bench_bin_open/discovery.rs) · [Move and discard tests](../crates/ui-egui/examples/bench_bin_open/history_tests.rs)

#### Feature-scale measurements

These are copies of the 24,000- and 48,000-placement fixtures, with 1,200 and 2,400 file media items respectively. The original fixtures mark the media intentionally offline. For this run the copies used ordinary file links into a moved folder, so project-wide discovery exercised missing-file candidate search. One generated movie in the chosen folder matched a stored filename. Each trial compared the entire output to a full standard reopen, resolving stored paths against their respective document folders for comparison. Name Find returned 100 items, File Path Find returned one, and discovery reported every missing media item.

Each row covers open through result construction in a fresh process on local APFS. Time is the median of five trials after one warm-up, and RSS is the highest whole-process peak among those five. Cold starts with an empty private index directory. Warm starts with the previously built index. The OS file cache stayed enabled throughout. Project MB counts logical project-file bytes read, including manifest rechecks. Decodes count item records, including a second decode if the cold build and the query both needed the item. RSS includes rendering-library and process overhead. The OS denied writes under both project paths for every trial, and file inventories, sizes, modification times and SHA-256 digests matched before and after. **Project bytes written were zero throughout.**

**24,000 timeline placements**

| Operation | Open | Time (ms) | Project read (MB) | Item decodes | Peak RSS (MiB) |
|---|---|---:|---:|---:|---:|
| Name Find | Full .fcproj | 97.8 | 18.13 | 1,212 | 125.7 |
| Name Find | Modular, cold | 155.3 | 18.49 | 1,212 | 25.4 |
| Name Find | Modular, warm | 5.5 | 0.30 | 0 | 16.3 |
| File Path Find | Full .fcproj | 97.6 | 18.13 | 1,212 | 127.7 |
| File Path Find | Modular, cold | 298.4 | 36.59 | 2,424 | 112.5 |
| File Path Find | Modular, warm | 143.3 | 18.40 | 1,212 | 111.2 |
| Relink discovery | Full .fcproj | 104.7 | 18.13 | 1,212 | 129.4 |
| Relink discovery | Modular, cold | 192.7 | 19.57 | 2,412 | 31.0 |
| Relink discovery | Modular, warm | 41.0 | 1.38 | 1,200 | 22.6 |

**48,000 timeline placements**

| Operation | Open | Time (ms) | Project read (MB) | Item decodes | Peak RSS (MiB) |
|---|---|---:|---:|---:|---:|
| Name Find | Full .fcproj | 193.5 | 36.28 | 2,412 | 236.7 |
| Name Find | Modular, cold | 290.5 | 37.00 | 2,412 | 38.2 |
| Name Find | Modular, warm | 8.4 | 0.58 | 0 | 18.2 |
| File Path Find | Full .fcproj | 194.0 | 36.28 | 2,412 | 236.7 |
| File Path Find | Modular, cold | 574.3 | 73.23 | 4,824 | 211.7 |
| File Path Find | Modular, warm | 286.6 | 36.82 | 2,412 | 208.3 |
| Relink discovery | Full .fcproj | 207.6 | 36.28 | 2,412 | 243.3 |
| Relink discovery | Modular, cold | 373.5 | 39.16 | 4,812 | 44.1 |
| Relink discovery | Modular, warm | 80.4 | 2.75 | 2,400 | 29.8 |

Cold opens wrote a separate preview index of 162,919 or 326,121 bytes. Warm opens wrote zero index bytes. Warm name searches decoded zero item payloads. Warm relink skipped the large sequence records. File Path Find used the general metadata fallback and was slower than a full standard reopen at both sizes. Cold index creation also cost more time than a full standard reopen in all six comparisons.

### Applying a verified source relink

Project-wide Find can select a missing media item for Link Media. Search for the replacement, then choose **Review in Source Bin…**. The editor opens or focuses the existing owning bin, selects the source, and shows the replacement path. Locate uses the same review after choosing a file. Opening the bin and reviewing the candidate leave the project clean. **Apply Relink** runs in the fully loaded local editor. The developer preview continues to offer discovery only.

- **Eligibility.** One source must have a recorded fingerprint and one owning bin in both the saved and working projects. A timeline reference, subclip, unfiled item, or ambiguous bin membership cannot authorize Apply. Project-wide search provides navigation to the direct source. Apply checks the reviewed revision, ownership, selection, old path, new path, fingerprint, and media properties again.
- **Effects.** A verified single-source relink creates one undo step. Only its owning bin becomes Unsaved. If an edit would also change a sequence anchor or Script record, the bin operation stops before changing the project or decoder. The dialog identifies a dependent record and offers an explicit full-editor relink, which rechecks the reviewed selection before including dependent edits.
- **Save and Discard.** Undo, Redo, and bin Discard restore the corresponding playback source and missing-media status. Bin Save publishes the owning bin and leaves unrelated edits outstanding. Global Save also publishes other dirty bins and changed settings. A newer publication before Apply requires fresh review against the current project. After Apply, a stale Save or Discard preserves the working edit and the newer publication.
- **Developer commands.** Modular `media.search` and the preview’s `relink-discovery` return the saved revision and the owning bin for each result. An eligible candidate includes a `review` object. The full editor accepts that object through `media.relink` with the selected `item`, `bin`, and top-level `path`. Direct file selection uses `reviewOnly: true` and the observed `revision` to obtain a review. Automation supplies the bin context explicitly.

The regression fixture imports a generated still, places it in a sequence used by a master, and moves its folder. The UI test starts from project-wide search, opens the owning bin once, applies by keyboard, and compares rendered frames after Undo, Redo, bin close and reopen. Other cases cover unrelated edits, global settings, a replaced candidate, changed ownership, missing fingerprints, a dependent sequence anchor, and two stale-save windows. [Verified relink tests](../crates/ui-egui/tests/it/modular_bin_ui/verified_relink.rs)

### Predicting the impact of a verified relink

The developer harness accepts `relink-impact project.fcmod REVIEW_JSON` with the verified candidate returned by `relink-discovery`. Its report identifies the saved revision, source, changed item IDs, and every bin that owns each changed item. The `unfiled` list identifies changed records outside the bin tree. A `script-review` result identifies the blocking Scripts and reports zero changed records, matching the full editor’s refusal.

The private index tracks subclip parents, Script take sources, and sequences with unresolved picture properties. A relink can fill in a frame-relative picture position even on an unrelated shot. The check loads those sequences, applies the editor’s record-update logic to an isolated model, and compares the records. A newer publication, a changed candidate or an unreadable required payload invalidates the result. The ordinary editor continues to handle Apply.

On disposable copies of each saved revision, `verify-relink-impact` reopens the full modular editor, performs the verified relink, and compares every changed record, bin membership, dirty bin, and unfiled change against the prediction. Regression cases cover nested cuts, chained subclips, Scripts with and without marks, an unrelated automatic position, and a sequence shared by two bins. The read-only check also preserves the open editor’s playback, Undo availability, missing-media list, saved view, and clean state. [Impact regression tests](../crates/ui-egui/examples/bench_bin_open/impact_tests.rs)

#### Feature-scale measurements

Both feature fixtures used the same generated still for the verified candidate. The source-only fixture retained its saved picture properties. The dependent fixture left one axis of the first placement’s source anchor automatic. The full modular editor changed just the source in the first case and both the source and its sequence in the second. The full-editor baseline below reopens the paired standard `.fcproj` and computes the equivalent impact.

For reproduction, pass the same discovery review to `relink-impact-baseline` on the paired standard fixture and to `relink-impact` on its modular manifest. Point `FILMCRAFT_DATA_DIR` at a fresh external directory for each cold trial and reuse that directory for the warm trial. Run `verify-relink-impact` on a disposable copy for the actual full-editor comparison.

Times are medians of five fresh-process trials after one warm-up on local APFS. Cold and warm refer to the private index. The OS file cache stayed enabled. Project bytes count logical reads including manifest rechecks. The decode count includes the cold index build. Peak RSS is the highest process peak of the five trials. The OS denied writes under the project directories for every measured run. Inventories, sizes, modification times, and SHA-256 digests matched before and after. **All measured checks wrote zero project bytes.**

| Placements | Change | Route | Median ms | Project MB read | Item decodes | Peak RSS MiB | Cache bytes written |
|---:|---|---|---:|---:|---:|---:|---:|
| 24,000 | Source only | Full .fcproj | 103.5 | 18.13 | 1,212 | 130.2 | 0 |
| 24,000 | Source only | Cold index | 257.8 | 18.58 | 1,213 | 27.1 | 183,523 |
| 24,000 | Source only | Warm index | 7.7 | 0.39 | 1 | 18.3 | 0 |
| 24,000 | Source + sequence | Full .fcproj | 101.8 | 18.13 | 1,212 | 135.5 | 0 |
| 24,000 | Source + sequence | Cold index | 168.5 | 20.00 | 1,214 | 34.2 | 183,537 |
| 24,000 | Source + sequence | Warm index | 16.1 | 1.80 | 2 | 33.9 | 0 |
| 48,000 | Source only | Full .fcproj | 200.3 | 36.28 | 2,412 | 240.4 | 0 |
| 48,000 | Source only | Cold index | 299.3 | 37.18 | 2,413 | 40.1 | 367,125 |
| 48,000 | Source only | Warm index | 9.3 | 0.76 | 1 | 20.2 | 0 |
| 48,000 | Source + sequence | Full .fcproj | 201.9 | 36.28 | 2,412 | 255.1 | 0 |
| 48,000 | Source + sequence | Cold index | 315.6 | 40.01 | 2,414 | 52.7 | 367,139 |
| 48,000 | Source + sequence | Warm index | 29.4 | 3.60 | 2 | 52.7 | 0 |

**Rollout decision.** The warm index passed the accuracy and speed gates at both sizes for both operations. A warm, verified source-only change qualifies for planning partial Apply. A warm dependent result can direct the user to full-editor review. Cold index creation took longer than the full reopen in all four comparisons, so cold checks stay on the full-editor route. The next milestone is to validate one source-only edit through Apply, Undo, bin Save or Discard, and reopen, including a newer publication between prediction and Apply.

### Applying one source-only relink with a warm index

In the local developer harness, a verified source-only relink makes its owning bin Unsaved. Apply, Undo, Redo, and Discard write zero project-content bytes. Saving the bin publishes one new source record, archives the previous manifest for recovery, and atomically replaces the head. A second Save of the same working state creates zero additional backup versions.

On a disposable copy, pass the discovery candidate’s `review` object and reuse the external data directory containing its warm index.

```sh
"$bench" relink-edit "$manifest" "$review" apply,undo,redo,save
"$bench" relink-edit-baseline "$disposable_project" "$review"
```

Each invocation has its own working session. For a manual concurrency test, pass `-` as the action list and enter one action per line. The working edit survives a failed Save or Discard while that process stays open. A newer publication before Apply requires fresh discovery. Cold or stale indexes, an original that came back online, and dependent changes require the full editor. Marked Scripts retain their review requirement. Ordinary preview stays read-only.

Apply pins the reviewed manifest under the existing writer lock, rereads the required records, repeats the impact and fingerprint checks, and runs the editor’s relink operation. Save verifies ownership and the saved before-image, overlays the changed fields on the original source record, and hashes the remaining immutable records before carrying their keys forward. Unknown fields survive on the rewritten source. Unopened bin contents, saved views, settings, and move-copy identities keep their existing manifest entries. Bin-backup accounting and retention use the existing revision mechanism.

The regression test compares rendered frames, source identity, missing-media status, Undo, and bin dirtiness with a full editor opened on a separate copy of the same revision. It reopens both the saved and discarded cases and rescues a nested cut from the bin backup to check that its source still resolves to the original path. A failed archive write leaves the old revision readable. Retrying publishes once. A newer save leaves the pending edit available and the newer head intact. [Regression fixtures](../crates/ui-egui/examples/bench_bin_open/source_edit_tests.rs)

#### Feature-scale measurements

Each route ran in a fresh process on copies of the feature fixtures from the impact check, using the same reviewed media. One warm-up preceded five timed trials per route, with rotating route order, local APFS and the OS file cache enabled. Open-to-save starts before opening the project and ends when publication returns. Apply and Save are separate median times. Peak RSS is the highest of the five process peaks. Each modular publication changed only the source item key, backed up one bin and retained the previous manifest. All three routes produced the same saved source record.

| Placements | Editor | Apply ms | Save ms | Open-to-save ms | Project MB read | Project bytes written | Item decodes | Peak RSS MiB |
|---:|---|---:|---:|---:|---:|---:|---:|---:|
| 24,000 | Partial, warm | 0.9 | 160.0 | 165.6 | 18.51 | 182,464 | 2 | 21.1 |
| 24,000 | Full modular | 13.8 | 73.0 | 389.7 | 18.40 | 182,464 | 1,212 | 540.3 |
| 24,000 | Full .fcproj | 7.4 | 195.3 | 295.1 | 18.13 | 18,129,962 | 1,212 | 149.5 |
| 48,000 | Partial, warm | 1.2 | 288.7 | 297.3 | 37.05 | 360,066 | 2 | 26.8 |
| 48,000 | Full modular | 30.6 | 100.9 | 736.3 | 36.82 | 360,066 | 2,412 | 1065.2 |
| 48,000 | Full .fcproj | 17.1 | 387.5 | 583.2 | 36.28 | 36,284,849 | 2,412 | 279.9 |

Separate untimed runs counted successful file I/O under the project directory, excluding user data and candidate media. Apply → Undo → Redo → Discard recorded zero project bytes written at both sizes. The partial path decoded the source once at Apply and once at Save. Save still read and hashed every retained payload. For these fixtures the partial and full modular editors wrote the same number of project bytes.

**Rollout decision.** Warm source-only relinks passed the correctness and open-to-save speed gates against both reference editors at both sizes. They qualify for planning editor integration. Save alone was slower than in an already-open full modular editor, which can reuse verified file stamps. Retain the full-editor route for already-loaded projects, cold indexes, and dependent changes. The next integration milestone is routing this qualified case from project-wide search while preserving bin close, global Save, and background checkpoint behavior.

### Crash recovery after a partial relink

In the local developer harness, run `relink-edit` with `apply,checkpoint` to exercise the first recovery stage. The worker verifies the saved revision, loads its complete project, and substitutes the working source record. It acknowledges the checkpoint after publishing a self-contained draft in the recovery directory. The owning bin stays Unsaved. A stale revision fails the checkpoint and preserves the previously acknowledged draft.

A normal project open shows the last explicit Save. Recover Unsaved Changes opens a detached draft and checks the chosen file against FilmCraft’s recorded media fingerprint before playback. An unreadable, missing, or fingerprint-mismatched file stays offline at the chosen path and opens Link Media for review. Saving the detached copy preserves the path, clip name and offline state. A second crash during editing retains the review requirement. An acknowledged Discard clears the draft.

The force-quit regression covers unchanged footage, a moved file, and different playable footage at the same path. It removes access to the original project folder before recovery, checks an unopened sequence and project settings, and reopens the saved copy in a fresh process. The next stage connects this opt-in path to desktop Find and verifies its bin-close and global Save behavior. [Recovery regression](../crates/ui-egui/examples/bench_bin_open/source_recovery_tests.rs)

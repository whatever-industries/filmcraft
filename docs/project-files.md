# Project files

FilmCraft projects are `.fcproj` files: UTF-8 JSON with an explicit schema version. Reading and
writing them is `crates/format` (`filmcraft-format`); the engine's `file.*` commands use it.

## Format

```json
{
  "format": "filmcraft.project",
  "schema_version": 17,
  "generator": "FilmCraft 0.12.0",
  "project": { "name": "…", "settings": { … }, "root": { … }, "items": { … }, "next_id": 48 },
  "view": { "open_sequences": [21, 50], "active_sequence": 50, "sequences": { "21": { "pps": 40.0, "scroll": 0.0, … } } }
}
```

- `schema_version` is the version of the whole document. This build writes
  `filmcraft_format::SCHEMA_VERSION` and reads every version from 1 up to it.
- `project` is the serialized `filmcraft_project::Project` (bins, media clips, sequences, tracks,
  clips, effects, keyframes, markers). Media is referenced by path, never embedded.
- Files are written compact (no indentation). A 2,000-clip project is about 1.4 MB.
- `view` (optional) is what was open when the project was saved: the Timeline's sequence tabs in
  order, the active one, each sequence's zoom, scroll and track heights, and each sequence's
  playhead (`filmcraft_project::ProjectView`). It is beside the project, not in it: it is not part of the
  edit, never an undo step, and changing it does not mark the project as changed. Opening a
  project restores it when the Timeline preference "Restore open sequences when opening projects"
  is on. It is outside the schema version: older builds ignore it, ids that are not sequences and
  numbers out of range are dropped or clamped (a playhead also snaps to a frame of its sequence), and a `view` that cannot be read is ignored (the
  project opens on its first sequence, as it does without one or with no open sequence in it).
- A coordinate of an effect's point parameter may be `null`. Point parameters use NaN for "auto"
  (the frame centre, or the source centre for `anchor`), and JSON writes NaN as `null`; it reads back
  as "auto". Placing a clip resolves these points at once. An interchange import resolves them
  too, except the `anchor` of a clip whose media file was not found. That one stays "auto", and
  the renderer centres it, until Link Media (`media.relink`, `media.autoRelink`) reads the file
  and with it the real picture size; a file that merely comes back at its old path does not
  change it. `null` anywhere else is a damaged file.

### Schema history

| Version | Since | Shape |
|---|---|---|
| 1 | M0 | the bare `Project` object with a `"version": 1` field |
| 2 | M11.1 | the envelope above; the project's own `version` field is gone |
| 3 | M10.2 | graphic clips (`ItemKind::Graphic` with text/shape layers); a no-op step for older data |
| 4 | M7.4 | Essential Sound: clip audio types and settings; no-op step |
| 5 | M8.8 | colour management: working space, Interpret Footage colour space, LUT library; no-op step |
| 6 | M11.7/M11.9 | media identity fingerprints, ingest settings; no-op step |
| 7 | M5.5 | effect masks: `masks` on effect instances with keyframable Bézier `Path` values; no-op step |
| 8 | M12 | multi-camera source sequences and clips, merged clips; no-op step |
| 9 | M10.5 | transcripts of media items (`project.transcripts`); no-op step |
| 10 | M3.10 | clip time interpolation, Hold Filters, Field Options, audio source channels, Modify ▸ Audio Channels map, subclip Restrict Trims; no-op step |
| 11 | M3.11 | search bins (`project.search_bins`), Flash Cue markers, Project Settings safe areas, capture format and scratch disks; no-op step |
| 12 | M10.7 | graphics design data: `TrackItem::graphic` (roll / crawl, responsive time, template link), `EffectInstance::layer` (layer uid, per-character styles, responsive pins), `project.source_graphics`; no-op step |
| 13 | Multi-stream audio | `MediaInfo::audio_streams` and `TrackItem::audio_stream`; older `audio` objects load as stream zero, missing clip indices default to zero; no-op step |
| 14 | Narrations | Text-to-Speech narration data; existing projects need no changes; no-op step |
| 15 | Project notes | Project notes default empty; no-op step |
| 16 | Marker and custom colors | Marker authors, custom marker/clip colors and named project palettes; existing presets remain unchanged and new fields default empty; no-op step |
| 17 | Marker URLs | Marker URLs default empty; older builds refuse v17 instead of silently discarding saved URLs; no-op step |

### Migrations

Older files are upgraded on load, in memory, by a chain of single-step functions
(`filmcraft_format::MIGRATIONS`, where entry *i* upgrades v*i+1* to v*i+2*). Each step rewrites the JSON
document; after the last one the result is deserialized into the current model. To change the schema:

1. add a function `vN_to_vN1(doc: Value) -> Result<Value, String>` and append it to `MIGRATIONS`
   (`SCHEMA_VERSION` follows automatically);
2. add a small hand-made fixture of the old shape to `crates/format/tests/fixtures/` and a test that it
   loads;
3. never edit a migration that has shipped.

Fields added with `#[serde(default)]` don't need a migration: missing values take their defaults.
But when new data would make the file unreadable to an older build (a new enum variant such as
`ItemKind::Graphic`, a new required shape), bump the schema with a no-op step so older builds refuse
the file with the message below instead of failing with a parse error. Do the same when an older
build would load the file but silently drop new fields on its next save.

When you open an older file FilmCraft says so ("Upgraded project from schema v1 to v2"). The file
on disk is unchanged until you save. The **first save over it** keeps the original next to it as
`<name> (schema v1 backup).fcproj`, so an older FilmCraft can still open your work.

### Newer files

A file with a `schema_version` newer than the build supports is **refused**, not half-read:

> this project was saved by a newer version of FilmCraft (project schema v12); this build reads up to
> v11. Update FilmCraft to open it.

Reading it partially and saving it back would silently drop whatever the newer version added.

## Developer project-folder converter

The [modular roadmap](modular-roadmap.md) tracks verified capabilities, prototype workflows and the remaining desktop and shared-storage milestones.

To try the modular fork from a fresh checkout:

```sh
git clone --branch modular-integration https://github.com/whatever-industries/filmcraft.git
cd filmcraft
cargo test --locked -p filmcraft-format
cargo run --locked -p filmcraft-format --example project_folder -- pack crates/format/tests/fixtures/v1-edit.fcproj edit-folder
cargo run --locked -p filmcraft-format --example project_folder -- unpack edit-folder restored.fcproj
```

The fork pins the schema-17 record library to an exact Git commit. Stock FilmCraft can open
the exported standard project. Keep builds on your preferred volume with `CARGO_TARGET_DIR`.

Use the native `project_folder` example to split a saved project into a new directory or
rebuild a standard project file from that directory. Both destination parents must exist.

```sh
cargo run -p filmcraft-format --example project_folder -- pack edit.fcproj edit-folder
cargo run -p filmcraft-format --example project_folder -- unpack edit-folder restored.fcproj
```

Each bin and project item (including each sequence) has its own JSON record. The
`project.fcmod` manifest maps stable project IDs to filenames. Each filename is the
lowercase SHA-256 digest of its contents. A separate record retains the project envelope,
view, and item inventory. Bin order, unfiled items and extension fields survive conversion.
Packing an older supported standard project applies the ordinary in-memory schema migrations.

The converter rebases relative original-media and proxy paths against each destination
document so offline media can still be relinked. It reads only project data. Media files
stay where they are. Moving a completed folder later can change what its relative paths
resolve to, just as moving a standard project can.

Packing reserves a fresh directory, reads the records back, reconstructs the project with
FilmCraft's standard reader, and publishes the manifest last. An interruption can leave
records in the new directory. A folder without a manifest is incomplete. Retry packing
the original project into a fresh destination. Unpacking checks every referenced digest
and record ID before publishing a new standard file. Both commands refuse occupied
destinations and keep their inputs intact.

Folder layout version 1 accepts at most 100,000 bin and item records, a 16 MiB manifest,
and 256 MiB of combined record data. A standard input or export must also fit FilmCraft's
ordinary project size limit.

### Incremental saves in the modular fork

`project_folder checkout FOLDER NEW_FILE.fcproj` exports the current snapshot and prints
its revision digest. Edit the exported file, then run
`project_folder save FOLDER BASE_REVISION EDITED.fcproj` with that digest. A successful
save prints the next digest. An unchanged save returns the same digest. A stale or busy
writer gets an error and retains its edited file.

On a local filesystem the writer holds an OS lock across the revision check and commit.
Changed records and the previous manifest are written first. Replacing the current manifest
commits the entire edit, including moves between bins. Changed bin and item records acquire
an internal UTC edit time and revision number. Imported edit times start unknown.
History accumulates in the folder. Each save still reads and validates the complete project.
Shared/network filesystem locking requires separate qualification.

### Edit a project folder from the local CLI

Open the manifest with the ordinary CLI commands. For example, inspect the bin IDs,
then rename one bin and save the edit in a single session:

```sh
filmcraft-cli --project edit-folder/project.fcmod inspect project
filmcraft-cli --project edit-folder/project.fcmod --save exec project.renameBin bin=1 name=Interviews
```

The first local editor holds the writer lock until it closes or switches documents.
A competing editor reports `readOnly` and the digest it opened in `project.inspect`.
It can inspect that snapshot or use `file.saveCopy` to export an independent new
`.fcproj`. `file.saveAs` opens the exported file as an ordinary project. Reopening
the folder after the writer exits acquires the lock. Folder sessions use manual saves.

Saving retains extra JSON fields on surviving records, including clips moved between
tracks and markers reordered by time. An edit that makes an extension's owner ambiguous
reports the affected record and field. The last published revision stays available and
the edited session can undo or revise the change and retry. The same rules apply to
standard exports from the folder session. Undoing a saved deletion restores the clip’s
extra fields along with its timing and edits. Saving again records that restoration as
a new revision. Redo in the same session can remove the clip again.

### Edit different bins in two local CLI sessions

Run one process per editor, with a separate data directory for each editor's drafts.
Both processes can open the same manifest on a local disk.

```sh
filmcraft-cli --shared-bins --data-dir ./editor-a --project edit-folder/project.fcmod run -
```

Use `project.inspect` to find bin and sequence IDs. For a bin with ID 70 containing
sequence 21, send these commands, one per line:

```jsonl
{"id":"project.checkoutBin","params":{"bin":70}}
{"id":"sequence.open","params":{"item":21}}
{"id":"project.renameBin","params":{"bin":70,"name":"Interviews"}}
{"id":"file.save"}
{"id":"project.releaseBin"}
```

Checkout locks the selected non-root bin and its directly filed sequences until release,
a successful switch, or process exit. A second editor can check out a different bin,
including a child bin. Whole-project writers require all checkouts to be released.
Each editor can rename the checked-out bin, reorder its existing entries with
`project.reorderBin`, and place, move, trim, split or delete clips in its sequences.
**Remove from Bin** unfiles source entries directly in the writable bin. Cuts retain their source records and can still play from the original or an enabled proxy. Undo restores the filing, including across Save. Deleting a whole bin hierarchy or moving entries between bins requires the whole-project editor. For example, `project.removeFromBin {"bin":70,"items":[5]}` removes item 5’s filing in bin 70. Under an exclusive writer, `{"bin":0,"bins":[70]}` unfiles the entire bin from the root.

Enable **Show Reference Clips** to reveal sources used by sequences in the bin. Two cuts using the same source produce one Reference row. List view shows the usual metadata columns. Right-click the row and choose **File in This Bin** to keep it visible when reference display is off. Removing this filing leaves the source in its other bins and both cuts still use it.

In modular projects, each filing points to the same source ID. Changing Comments, Scene, Take, Name or Label updates that field for every filing once the editor sees the revision. Clearing a value counts as an edit. The folder orders edits when accepted, so an editor who saves an older draft later cannot overwrite a newer field. **Duplicate** gives the copy its own ID and editable metadata while sharing the footage. A standard project export keeps one home filing for each source and preserves both cuts.

Save publishes the owned records against the latest manifest. This preserves other editors'
completed saves. `project.refresh` loads their changes into a clean session and resets Undo
history. Undo and Redo work within the current checkout, including across manual saves.
A dirty session must save before switching bins or explicitly release with `{"discard":true}`.
Each checkout reserves a distinct range of project IDs. Gaps after Undo or release are expected.

Shared mode enables private crash checkpoints. After a crash, start with `--shared-bins`
and the same data directory. Use `file.recoveryList` and `file.recover` as described below.
Recovery can reopen the draft over a newer save to another bin. If the bin is busy, wait
for release. If an owned record changed or reserved IDs now conflict, export the draft
to a new `.fcproj`. Manual publication still requires an explicit save.
The multi-process tests cover local filesystems. Network-volume locking and desktop
collaboration are separate milestones.

### Recover unsaved project-folder edits

Start a local CLI session with recovery enabled. The data directory holds private checkpoints and should live outside the project folder.

```sh
filmcraft-cli --folder-recovery --data-dir ./editor-data --project edit-folder/project.fcmod run -
```

Send one JSON command per line. The background worker periodically checkpoints unsaved edits.
`{"id":"file.autoSaveNow"}` waits for the current folder checkpoint and returns its edit revision.
Other readers see the last manual save until you run `file.save`. A writable folder owns its
own journal, including when two projects have the same name.

After an interruption, use the same data directory to find and recover the draft:

```sh
filmcraft-cli --folder-recovery --data-dir ./editor-data exec file.recoveryList
filmcraft-cli --folder-recovery --data-dir ./editor-data run -
```

In the second session, send `{"id":"file.recover","params":{"id":"RECOVERY_ID"}}` to open
a dirty draft and `{"id":"file.save"}` to publish it. If the original folder moved, has a
newer save, or has another writer, add `"path":"rescued.fcproj"` to the recovery parameters
to write an independent file. Choose an unoccupied destination. The checkpoint preserves
project metadata, clip extensions and media references. The media files remain in place.
`file.discardRecovery` with a candidate ID discards that draft.

### Mixed-format native engine sessions

A native host can opt in with `Session::local_project_editor(AutosaveConfig)`. Pass the
desktop data directory, local-time callback and debounce interval. The session loads the
usual preferences. Use `file.open` with either a standard project or a folder's
`project.fcmod`. Standard files use the ordinary autosave settings. Folder edits go to a
private checkpoint until `file.save` publishes them. On a busy folder the session opens
the published revision read-only. `file.saveCopy` exports to a fresh standard file and keeps
the folder open. `file.saveAs` exports and continues editing the new file.

When switching documents, the engine validates the destination and confirms a dirty
outgoing draft before releasing ownership. If standard recovery journaling is disabled,
save a dirty standard file before switching. `file.recoveryList` tags each draft with
`storage` and `id`. When both stores have candidates, supply both fields to recover or
discard one draft. For example:

```json
{"id":"file.recover","params":{"storage":"projectFolder","id":"RECOVERY_ID"}}
```

Add `path` to export a folder draft when in-place recovery is unsafe. To discard every
candidate of one kind, pass `storage` and `all: true` to `file.discardRecovery`.

### Editing project folders in the fork desktop app

Choose **File → Open Project Folder…** and select the directory containing
`project.fcmod`. Open a non-root bin in the Project panel, or open one of its
sequences in the Timeline. The bin header shows **Writable** when this session owns
the bin and its directly filed sequences. A busy bin opens **Read-only**. To try for
ownership after another editor releases it, close the bin and open it again.

Open several bins to keep independent edits and Undo histories. **Save** publishes
the focused bin. **Save All** publishes each dirty bin. **Close Bin** publishes its
edits and releases its lease. Closing a sequence tab leaves its bin open. Switching
projects or quitting saves the open bins in ID order. If a save fails, the editor
reports the bin ID and keeps the document and its leases open. Earlier successful
bin saves remain published. Correct the cause and retry.

**File → Export Standard Project…** creates a new independent `.fcproj` from the
latest published project and the open bin drafts. After a crash, select a draft by
project, bin name, ID and time. **Recover** reopens that bin for editing. **Recover to
File…** writes an independent copy when the bin is busy or its published content
has diverged. Use a new filename outside the project folder.

This workflow targets processes on a local disk. The root is a browsing view.
The supported edits include renaming and reordering a checked-out bin and editing
its directly filed sequences. Source records and project-wide structure have
separate ownership requirements.

#### Two-editor desktop trial

On a single Mac with a local APFS volume, two native desktop instances with separate
profiles edited different cuts in the same folder. The smaller fixture had two bins
and two clips. The larger had 48 bins, one sequence per bin, and 1,152 clips sharing
one offline source and proxy. The fork revision was `1a346037`. The independent
reader was an unmodified FilmCraft 0.20.1 desktop built from `2d1d1b69`.

| Exercise | Verified result |
| --- | --- |
| Trim different cuts, save A then B, and reverse the save order on the larger fixture | The standard export contained both trims and preserved every other clip. |
| Open A’s bin in B, try a trim, then release in A and reopen in B | B showed **Read-only** and refused the trim. After handoff it showed **Writable** and edited the previously saved cut. |
| Checkpoint A, kill its process, and save another cut in B | Restarting A recovered its private edit. B continued to see the published version until A explicitly saved. Both cuts then appeared in the export. |
| Have B acquire the crashed editor’s bin before recovery | In-place recovery refused the busy bin. Recovery to a new file reproduced the draft at checkpoint time. The shared project kept B’s later edit. |
| Open the published and recovered files in stock FilmCraft | Stock reported the expected clip and source IDs, starts, durations and source in-points. Original and proxy references still resolved to the same offline locations in a separate stock save. |

The modular exports retained the fixture’s unknown clip fields. Stock FilmCraft
0.20.1 omitted those fields when it wrote its own copy. Keep the modular project
or its original export when those fields matter. Network leases, multi-machine
reconnect and simultaneous edits within a sequence need separate trials.

### Compare two saved revisions

Use two full revision digests from `project_folder history FOLDER`. For a deleted bin,
run `project_folder list-bins FOLDER FROM` on an earlier revision to find its ID.

```text
project_folder compare-bin FOLDER FROM TO BIN_ID
project_folder compare-sequence FOLDER FROM TO SEQUENCE_ID
project_folder rescue-bin FOLDER CHOSEN_REVISION BIN_ID NEW.fcproj
```

The bin report follows descendants, matches stable IDs, and lists changed sequences
by name and ID. Drill into a sequence to see clip and source IDs, track IDs, start,
duration, and source in-point on each side. Times are exact FilmCraft ticks. Timeline
timecode uses that revision’s sequence rate, drop-frame setting, and start timecode.
Source in-points also have a display value at sequence rate. A source-only relink
produces a dependency row. The `other` change covers stored properties outside
the detailed edit list, including unknown fields.

Both comparison commands emit JSON lines in deterministic order, from `FROM` to
`TO`. A member removed from the selected bin may still exist elsewhere in the
project. The removal row’s `absentFromProject` flag distinguishes these cases.
A final `complete: true` row and exit code zero indicate a verified report, even
when the revisions differ. If a required record is unreadable, inspect the error
rows for its ID and revision. The report retains verified findings and exits nonzero.
An invalid revision or unreadable manifest can stop comparison before a report is
available. Comparison reads saved project metadata and works with offline media.

### Pin a milestone save

Choose a revision digest from `project_folder history FOLDER`. Give that exact save a label so another editor can find the same cut after later saves.

```sh
project_folder pin FOLDER REVISION "Picture lock"
project_folder list-pins FOLDER
project_folder rename-pin FOLDER REVISION "Picture lock" "Approved cut"
project_folder unpin FOLDER REVISION
```

`list-pins` prints JSON entries with a revision digest and label. In **Project History** and **Bin History**, pinned revisions show their milestone label beside the revision number. Two saves can share a label and still open independently. Returning to the revision list picks up label changes. If the labels cannot be read, you can still browse the numbered saves. Repeating the same pin is safe. Use `rename-pin` to change a label while keeping the revision pinned. Supply the exact current label from `list-pins`. If the stored label differs or the pin is missing, the command reports the conflict and preserves the current state. Removing a pin leaves the save available in history.

Pins live in `revision-pins.json` alongside the project manifest. A separate local lock serializes pin changes while an editor has the project open. Pinning leaves the published revision and any private edits intact. All published history is retained today. A future retention tool must honor these pins.

### Preview history retention

In a modular project's Project panel, open **Project History → Preview History Retention**. The first preview keeps the latest save plus pins and local dependencies. Adjust **Recent saves to keep** and choose **Run preview** to try another policy. The scan runs in the background. The report counts distinct revision bases held by the current editor, open bins and Undo/Redo, and matching checkpoints in the current recovery profile, including journals left by an interrupted session. Other computers, profiles, and unsaved work in other editors can have additional dependencies.

The desktop engine query `project.retentionPreview` accepts `{"keepLatest":1}` and returns the per-save and per-file report. It uses the active project and profile. A checkpoint with unreadable metadata can prevent the scan from completing because the editor cannot establish which project it belongs to.

For a standalone inspection, run `project_folder retention-preview FOLDER KEEP_LATEST` to see what depends on each stored record. Use a positive number of recent saves, including the current one. Milestones are included automatically. Add `--reader REVISION` for each known reader or checkout base and `--draft SESSION_DIRECTORY` for each known private checkpoint. To include every matching checkpoint from another local FilmCraft profile, pass `--profile DATA_DIRECTORY` with the profile’s data directory. The tool looks under `FolderDrafts/Recovery`. Repeat the option for each profile you want to inspect, including the active one. For example:

```sh
project_folder retention-preview MyProject 3 --profile StudioProfile --profile LaptopProfile
```

The tool reads the checkpoint descriptors, including those from running sessions, and verifies matching drafts against the original folder’s identity. An independent backup can share revision hashes and still have its own drafts. A missing recovery store or unreadable descriptor in a requested profile makes the command fail. A successfully scanned empty store contributes no holds. Unsaved edits that have yet to reach a checkpoint can still depend on other revisions. Use `--reader` for known reader bases. All three options are repeatable.

The JSON lists saves newest first, their pin labels, and why each would be kept. For each stored member it reports the logical size, how many saves share it as a payload, whether it belongs to the manifest chain, and which holds apply. Each file counts once toward these totals:

- `retained_policy_bytes` covers the manifest chain and the payloads needed by the selected saves.
- `older_only_payload_bytes` measures payloads used exclusively by other published saves. Dropping these bytes today would break those saves.
- `unreferenced_digest_bytes` counts regular files with digest-shaped names outside the captured chain. They may belong to a save in progress or contain unrelated data.
- `confirmed_reclaimable_bytes` is zero under the current storage format. A future pruning operation needs a way to retire saves while preserving history traversal and account for every active reader and recovery base.

The preview checks payload digests and fails if protected history is missing or the head or pins change during inspection. Reader revisions and private checkpoints outside the supplied inputs are unknown. Sizes exclude private draft payloads, sidecars, and external media. Filesystem allocation, compression and cloning can affect physical space.

### Back up published history

Use a new folder on your backup volume. Copy footage and proxies separately if you need media playback after losing the source drive.

```sh
project_folder backup FOLDER NEW_BACKUP_FOLDER
project_folder history NEW_BACKUP_FOLDER
project_folder rescue NEW_BACKUP_FOLDER REVISION SEQUENCE_ID recovered.fcproj
```

The first command prints the captured head digest. It captures the milestone pins at the start of the backup, then copies the published project metadata and every ancestor of the captured head. It verifies each pinned digest occurs in the copied history. Changes to the pins after capture apply to the next backup. Shared records occupy one file per digest. The backup reads every saved project from the destination before publishing `project.fcmod`. Another editor can keep working and saving during the copy. To include an editor’s draft, have them save it before starting the backup.

The resulting folder opens directly as a modular project. It retains revision IDs, original and proxy path strings, and embedded project data. Relative media paths resolve from the restored project folder. Restore the original folder layout or relink the media if you move it. Private checkpoints, locks, unrelated files and external media are excluded.

Choose an unused destination outside the source folder. An error identifying a historical item can mean that an older save is damaged even when the current project still opens. If the process is interrupted, a destination without `project.fcmod` is incomplete. Retry with a fresh folder.

### Check originals and proxies after restore

Run `project_folder media-availability FOLDER REVISION` with a digest from `history`. The JSON report lists each media item once by ID, including unfiled sources. Its counts summarize which originals could be opened for reading, which items have only a readable proxy, which need relinking, which need inspection, and which are generated. Each row includes the saved paths and the editor’s Make Offline flag. An intentionally offline clip can still have a readable original.

Paths are checked on this computer relative to the selected folder. A missing file, a path FilmCraft refuses to follow, and an access error have separate details. The check opens files read-only without decoding, fingerprinting or relinking them. For an image sequence it checks the stored frame path only. Media availability can change between runs on the same revision.

Missing media is a valid result. If an item record is damaged, the command prints the readable rows and errors by item ID, then exits unsuccessfully. An unreadable revision or project envelope prevents the inventory from starting.

### Recover a sequence

Run `project_folder history FOLDER` to list revision numbers and digests, newest first.
Use `project_folder restore-copy FOLDER BASE_REVISION HISTORICAL_REVISION SEQUENCE_ID`
to add the old cut alongside the current one. The result prints the new project item ID
and revision. The copy goes into the current cut's bin, or the root if the original
was removed. Existing media and nested sequences keep their IDs. If a dependency or
its attached data changed, or the project LUT library differs, use a separate export.

`project_folder rescue FOLDER HISTORICAL_REVISION SEQUENCE_ID NEW_FILE.fcproj` writes
a standard project with the selected cut, its transitive sources and a fresh root bin.
It follows subclips, nested sequences, multicam angles and merged sources. File-backed
media and proxy references are rebased to the destination. The root manifest, requested
history chain, envelope and required item payloads must still be readable. Unrelated bins
and items can be missing. This command leaves the damaged folder available for further
recovery. Keep a separate backup for loss of the underlying volume.

### Find sequences that use a saved item

In **Bin History**, right-click an item and choose **Find Uses**, or select one row and use the **Find Uses** button. Each result names a sequence that uses the item directly or indirectly. Choose its bin path to select the sequence in that same saved revision. From there, **Copy Selected Items** uses the ordinary history clipboard. The first search scans the selected revision. Subsequent searches in that revision use the same results. Choose **Refresh** to reread the saved records after damage or a repair. An incomplete scan shows the matches it could verify. Closing Bin History, leaving the revision, or opening another project releases the index. Opening a result and recovering items still read the required records from disk.

For the command line, choose a revision digest from `project_folder history FOLDER` and the project item ID of the clip, subclip or sequence.

```sh
project_folder find-uses FOLDER REVISION ITEM_ID
```

The JSON report lists each matching sequence once. `direct: true` means the sequence itself references the selected ID through a timeline clip, a multicamera angle or a merged input. An indirect match reaches the item through another sequence or a subclip’s parent. Duplicate clips that share a media path keep their own IDs. The `filings` list shows which bins explicitly contain the selected item or a matching sequence, with bin IDs and saved paths.

The search reads saved project records and works with offline media. Check `complete` before interpreting an empty result. If a bin or item is unreadable, the command prints verified matches and errors keyed by record ID, then exits unsuccessfully. In an open modular project, automation can request the same report with `project.findSavedItemUses` and parameters `{"revision":"<digest>","item":5}`. A read-only editor can run it while another editor owns the folder.

### Find and rescue a saved bin

Use a digest from `project_folder history FOLDER`. To find a bin that has since
been renamed or deleted, choose a revision from before the change.

```sh
project_folder list-bins FOLDER REVISION
project_folder preview-bin FOLDER REVISION BIN_ID
project_folder rescue-bin FOLDER REVISION BIN_ID NEW.fcproj
```

The listing prints one JSON object per line. Each readable bin has an `id`,
a `name`, and a `path` array from the root through that bin. Use the ID when
names repeat. An unreadable bin has an ID and an error. A readable bin can
have a null path if its ancestry cannot be verified. An incomplete listing
exits unsuccessfully and still prints the entries it could read.

Preview prints one JSON inventory for that save. `bins` lists the selected bin and
its descendants, including empty bins. `filed_items` lists items explicitly filed
anywhere in that subtree, including secondary filings. `referenced_items` lists
additional items needed by those contents. Each list includes saved names and
IDs in ID order. Two cuts using the same external source bring it along once.
Preview reads project records, works with offline media, and needs no destination.

Rescue writes a new standard project outside the source folder. The selected
bin appears at the top level with its original child order, empty sub-bins,
and item IDs. Selecting the root retains it as the root. Sources used outside
the recovered hierarchy appear once under **Referenced Items**; a numbered
suffix distinguishes an existing name. Offline media and proxies keep pointing
to their original locations. The command reads the selected subtree and its
required records, so a damaged sibling or parent can be bypassed using the bin
ID. A missing required record stops the export before the destination is created.

### Import a saved bin from another modular project

In the Project panel, open the destination bin for writing, then choose **Import Saved Bin…** from the panel menu. You can also open the menu on a bin you already have checked out. Choose the other project’s folder, select a saved revision, then select a bin by its path and ID. The preview lists the bins, filed items and additional references. **Import** adds the selection to the destination. **Cancel** closes the preview. If the source publishes another save during review, choose **Refresh** to check the selected version again.

From the local project-folder CLI, use `project.previewExternalBin` with just `folder` to list saved revisions. Add `revision` to list the bins in that save. Call `project.previewExternalBin` with the source `folder`, saved `revision` digest and `sourceBin` ID. Pass those same values to `project.importExternalBin`, together with the preview’s `head` and `epoch` and the destination parent’s `bin` ID. The result gives the new bin ID. If the source publishes another save, preview again. You can still choose the earlier revision.

Import copies the hierarchy with fresh bin and sequence IDs. Off-bin dependencies remain available through Show Reference Clips. Original and proxy paths continue to resolve from the destination, including offline paths. Repeat imports from the same source folder can reuse a matching media clip or subclip. Changing its metadata in either project makes the next import keep a separate source. Moving the source folder also starts a separate identity for subsequent imports.

With desktop bin checkouts, open the destination bin for writing before calling the import command. Save publishes the imported hierarchy in one revision. Other editors can keep working in their bins and can read the published children. Release the destination checkout to let another editor check out an imported child independently. The whole-project writer also supports import.

Undo and Redo cover the entire import. Recovering a checkpoint requires the published subtree to still match the checkpoint and its child locks to be available. A conflicting draft can be recovered to a separate `.fcproj`.

## Saving is atomic

Every write of a project file, auto-save, recovery snapshot or preferences file goes through
`filmcraft_format::atomic_write`:

1. write the bytes to a hidden temp file in the **same directory** (`.<name>.<pid>-<n>.tmp`);
2. `fsync` the temp file;
3. rename it over the target (an atomic replace on every supported OS);
4. `fsync` the directory so the rename itself is durable.

A crash, `kill -9` or power cut at any point leaves either the complete old file or the complete new
one. If a step fails, the temp file is removed and the old file is untouched.

Atomic writes keep the file whole, but they don't merge: of two sessions that opened the same project,
the one that saves last replaces what the other saved. `filmcraft-cli --save` runs take turns on a lock
to avoid that ([agents.md](agents.md)); the desktop app and `mcp` sessions don't.

## Commands

| Id | Menu | |
|---|---|---|
| `file.save {path?}` | File ▸ Save (⌘S) | write to the project's path (or `path`) |
| `file.saveAs {path}` | File ▸ Save As… (⇧⌘S) | write and adopt the new path |
| `file.saveCopy {path}` | File ▸ Save a Copy… (⌥⌘S) | write a copy; the project keeps its path and unsaved state |
| `file.revert` | File ▸ Revert | reload the last saved version, discarding changes |
| `file.open {path}` | File ▸ Open Project… (⌘O) | returns `{schemaVersion, migrated}` |

In the UI, Save / Save As / Save a Copy without a `path` show a file dialog.
Open Project… shows one too. In the desktop app, when the current project has unsaved changes, it
then asks *"Save changes to 'X' before closing?"* with **Cancel**, **Don't Save** and **Save**, and
the chosen project opens after Save or Don't Save. File ▸ Open Recent and the Recent list on Home
ask the same way.

## Auto Save

Preferences ▸ Auto Save (Edit ▸ Preferences ▸ Auto Save…, ⌘,; FilmCraft ▸ Settings… on macOS) works
like Premiere's:

| Setting | Key | Default |
|---|---|---|
| Automatically save projects | `autoSave.enabled` | on |
| Automatically Save Every N minute(s) | `autoSave.intervalMinutes` | 5 (1–1440) |
| Maximum Project Versions | `autoSave.maxVersions` | 20 (1–1000) |
| Auto Save also saves the current project(s) | `autoSave.saveCurrentProject` | off |
| Keep a recovery copy of unsaved changes | `autoSave.recoveryJournal` | on |
| Update it at least every N second(s) | `autoSave.recoveryIntervalSeconds` | 5 (1–600) |

At each interval, if the project changed since the last auto-save, a copy is written to the
`Auto-Save` folder next to the project as `<name>-YYYY-MM-DD_HH-MM-SS-<unix seconds>.fcproj` (local time, then the real time that orders
the copies, so a clock change at the end of daylight saving time cannot reorder them). An
unsaved project uses `<data dir>/Auto-Save/`. After each write the oldest copies beyond the limit are
deleted. Only files named exactly `<name>-<timestamp>.fcproj` or `<name>-<timestamp>-<unix seconds>.fcproj` are ever deleted. An auto-save is an
ordinary project file: open it with File ▸ Open Project. `file.listAutoSaves` lists them, newest
first.

Project Settings ▸ Scratch Disks can send auto-saves (Project Auto Save), render previews (Video
Previews), captured and generated media (Captured Audio and Video, also the default for proxies)
and audio previews to another folder. A folder named in a project file is used only when it is
inside the project's own folder or was chosen in Scratch Disks on this computer (remembered in
`autoSave.scratchFolders`). Otherwise FilmCraft uses the default location and says so when the
project opens. Folders on another computer or a device are never used.

Preferences are stored in `<data dir>/preferences.json`. The `prefs.*` commands read and change them.

## Crash recovery

Premiere's auto-save can lose up to one interval of work. FilmCraft also keeps a **recovery journal**:

- Whenever the project has unsaved changes, a snapshot is written to
  `<data dir>/Recovery/<started>-<pid>[-<n>]/snapshot.fcproj`, plus `session.json` (time, revision, project
  name and path). It is written about 1 s after the last edit, or after at most
  `recoveryIntervalSeconds` while edits keep coming. Saving the project deletes the snapshot.
- Each running FilmCraft holds an OS file lock on `<session>/lock`. When the process dies (crash,
  `kill -9`, power loss) the OS releases the lock.
- At startup, every other session directory whose lock can be taken and that has a snapshot is a
  recovery candidate. FilmCraft asks: *"FilmCraft quit unexpectedly while 'X' had unsaved changes.
  Recover unsaved changes from 2026-10-01 00:02:09?"*, with the buttons **Recover**, **Not Now** and
  **Discard**. Stale directories without a snapshot are removed. Journals of running sessions are
  never touched.
- If discarding a recovery session fails, FilmCraft reports the error and keeps that session and
  the remaining candidates in the recovery list for retry. Sessions already deleted stay removed.
- If the auto-save worker stops unexpectedly, FilmCraft reports the failure once in the Events
  panel and an error notification; `file.autoSaveStatus` reports `running: false`. Edits remain in
  the open project. Use **File ▸ Save** or **Save As** to save them; the worker is not restarted
  automatically. Ordinary write errors are reported without treating a live worker as stopped.
- Recovering loads the snapshot as an **unsaved** project with its original path, so Save writes
  over the original file. The recovered state goes into the new session's own journal before the old
  journal is deleted. Both the replacement snapshot and its metadata must be successfully written
  for that recovered revision. If journaling is disabled, writing fails, or the worker cannot
  confirm the write, recovery reports an error and retains the original candidate. The recovered
  project remains open and unsaved; use Save or Save As to keep it.
- If removing the old recovery folder fails after the replacement is confirmed, the recovered project
  stays open and the candidate stays listed. The old folder may have been partly removed. Save or use
  Save As before retrying: if the old snapshot remains intact, another recovery can overwrite unsaved edits.
- A normal quit with nothing unsaved removes the session directory. Quitting with unsaved changes
  (Quit, Cmd+Q or the window's close button) first asks *"Save changes to 'X' before closing?"*
  with **Cancel**, **Don't Save** and **Save** (Save on a never-saved project opens the Save dialog);
  Don't Save discards the changes and their snapshot. A quit that does not ask (the control
  channel's `app.quit`, the OS ending the session) keeps the snapshot, marked `cleanExit`, and FilmCraft offers it on the next launch ("FilmCraft was
  closed while … had unsaved changes").

Desktop flags: `--recover` recovers the newest candidate without asking. `--no-recover` starts
without asking; the changes stay available through File ▸ Recover Unsaved Changes…. `--data-dir
<dir>` (or `FILMCRAFT_DATA_DIR`) moves the data directory. The default is `~/Library/Application
Support/FilmCraft` on macOS, `%APPDATA%\FilmCraft` on Windows, and `$XDG_DATA_HOME/filmcraft` or
`~/.local/share/filmcraft` elsewhere. A `portable.txt` file next to the executable (the Windows
portable zip ships one) makes it `data` next to the executable instead.

### Effect Presets

User effect presets live in `<data dir>/presets.json`. In addition, FilmCraft creates and scans
`<data dir>/Effect Presets` on startup (and whenever **Effects ▸ Presets ▸ Scan Presets Folder** or
`presets.scan` runs) for `.prfpset` (Adobe Premiere Pro Effect Preset, plain XML or gzip-compressed)
and `.json` preset packs. Dropping `.prfpset` files into `<data dir>/Effect Presets`, into the
application window, or importing them via **File ▸ Import…** / **Effects ▸ Presets ▸ Import Presets…**
loads their presets with their nested folder hierarchy, static parameters, animated keyframes,
Bezier curves, and keyframe scaling/anchoring modes.

### Cost

The UI thread only gives the worker thread an `Arc<Project>`: the immutable snapshot the engine
already keeps for undo, passed through a channel. JSON encoding and all file I/O run on the
`filmcraft-autosave` thread. Measured on the development Mac (release build) with a 2,000-clip project
(`cargo test --release -p filmcraft-format --test fixtures -- --nocapture`):

| Step | Time | Size |
|---|---|---|
| encode, compact (what is written) | 2.5 ms | 1.4 MB |
| encode, pretty (not used) | 7.1 ms | 5.6 MB |
| decode | 4.6 ms | |
| atomic write + fsync (APFS SSD) | 30 ms | |

In the running app with the demo project, a recovery write measured 0.1 ms to encode and 27 ms to
write (two fsynced files).

## Commands

| Id | |
|---|---|
| `file.recoveryList` | candidates: `id`, `projectName`, `projectPath`, `savedAt`, `revision`, `cleanExit`… |
| `file.recover {id?}` | recover (newest when no `id`). From the UI, `{}` opens the recovery dialog |
| `file.discardRecovery {id?, all?}` | delete a candidate's journal |
| `file.autoSaveNow` | write an auto-save now (if anything is unsaved) |
| `file.autoSaveStatus` | prefs, folders, last auto-save / journal times, last encode/write ms and bytes |
| `file.listAutoSaves` | auto-save files of the current project, newest first |
| `prefs.get {key?}` / `prefs.set {key, value}` or `{values:{…}}` / `prefs.reset {category?}` | settings (below) |

UI automation ids: `recovery.recover`, `recovery.later`, `recovery.discard`, `recovery.item.<n>`;
`revert.yes`, `revert.no` (File ▸ Revert asks first when invoked without params).

## Settings

Premiere's Settings dialog (app menu ▸ Settings ▸ <category> on macOS, Edit ▸ Preferences
elsewhere; General is Cmd+,; the window is titled "Preferences"). The values are user preferences,
not project data: they live in `preferences.json` in the per-user data directory (written
atomically on every change) and never touch the `.fcproj` schema. The file carries a `version`
(currently 2); older files are migrated on load, unknown dropdown values, bad colours and
out-of-range numbers are repaired to defaults/limits, and an unreadable file falls back to the
defaults.

Every value is a dotted key whose first segment is its category: `timeline.stillImageDuration`,
`labels.colors.rose.name`. `prefs.get` / `prefs.set` / `prefs.reset {category?}` reach them from the
CLI, the control channel and MCP; `prefs.set` rejects values that are not one of a dropdown's
choices. `prefs.schema {category?}` lists every category with its fields (label, kind, choices,
range, unit, current value, and `wired`: whether FilmCraft acts on it yet). `app.settings.<category>`
opens the dialog on a page.

| Category | Takes effect |
|---|---|
| General | At Startup (Show Home = demo project, Open Most Recent, empty project; recent projects are remembered on open/save), Show Tool Tips |
| Appearance | Appearance Mode `appearanceMode` (`auto` = Sync with system / `dark` (default) / `light`; the header button and View ▸ Appearance ▸ Next Appearance Mode cycle it), Dark Theme `darkTheme` (`darkest` (default) / `dark`), Light Theme `lightTheme` (`light`), highlight colour, accessible contrast. `colorTheme` (Darkest / Dark / Light) is legacy input: setting it (older files and clients, View ▸ Appearance ▸ a theme) selects that theme and fixes the mode to its family; a file without `appearanceMode` migrates to its theme's fixed mode. It is kept in line with the choices: the theme a fixed mode shows, the dark theme in Auto |
| Audio | Automatch Time, Large Volume Adjustment, automation keyframe thinning (linear, minimum time) |
| Audio Hardware | device class (cpal host), output device, I/O buffer size, sample rate, force document rate, Output Mapping (programme L/R → device channels) |
| Auto Save | the auto-save ring and the crash-recovery journal |
| Color | stored only (display colour management, EDR monitoring, HDR graphics white) |
| Graphics | new text layers: smart quotes, ligatures, default font |
| Labels | the 16 label names and colours (timeline, Project panel, Edit ▸ Label), label defaults for imported movies / video / audio / stills and new sequences, Label Color Presets: JSON files (`{"name", "labels": [16 × {"name", "color": "#rrggbb"}]}`) in `<data dir>/Label Presets`, built in: "Default" (the labels FilmCraft ships with), "Bright" (16 saturated hues) and "Roles" (B-Roll, Footage, Dialogue… named so the label defaults land on matching roles) (`labels.presets`, `labels.importPreset {path}`, `labels.exportPreset {path}`, `labels.deletePreset {name}`; `labels.preset` is the selected one) |
| Media | Indeterminate Media Timebase (frame rate of stills), Default Media Scaling (Scale to / Set to frame size when a clip of another size is edited in), Enable proxies |
| Media Analysis & Transcription | auto-transcribe imported clips (scope "all imported"), speech model, default language / auto-detect, speaker labelling (defaults of `transcript.generate`) |
| Media Cache | location (`<data dir>/Media Cache` by default; unsaved projects' render previews live there), automatic deletion (older than N days / beyond N GB), Delete… (`mediaCache.clean {all?}`, `mediaCache.info`) |
| Memory | frame cache budget of the monitors and thumbnails |
| Playback | preroll / postroll (Play Around, trim loop), Step forward/back many |
| Plugins | stub (no plugins) |
| Timeline | video / audio transition and still image default durations (frames or seconds), playback auto-scrolling (none / page / smooth), snap playhead, return to beginning at playback end, play after rendering previews, Show Clip Mismatch Warning dialog (the first clip put into an empty sequence it doesn't match asks whether the sequence takes the clip's settings) |
| Trim | Large Trim Offset, Selection tool picks roll (on the cut) / ripple (on the edge) without a modifier, playhead determines trim loop |

The remaining fields are kept for parity and shown, but have no effect yet (`wired: false`).

UI automation ids: `settings.category.<id>`; `settings.<key>` for every control (for example
`settings.timeline.autoScroll`, `settings.labels.colors.rose.color`); `settings.<key>.<value>` for
the items of an open dropdown; `settings.<key>.browse`, `settings.<key>.hex`;
`settings.mediaCache.clean`; `settings.help`, `settings.reset` (this page back to its defaults),
`settings.cancel`, `settings.ok`. The open dialog's page and draft are `ui.settings` in `ui.inspect`;
`ui.set {"settings": {"page": id, "values": {key: value}}}` edits the draft (OK applies it).

## Media: offline, relinking, proxies, ingest

Media is referenced by path. Each imported file also records its **identity**
(`MediaClip::identity`): the size and a 64-bit fingerprint of the size, the first MiB and the last
MiB. Reading 2 MiB is cheap on any file, and it tells a moved original from a different take with
the same name. The field is optional (`skip_serializing_if`); it and `ProjectSettings::ingest` came with schema
v6, so builds that would drop them on save refuse the file instead. Projects from older builds
have no identity, so relinking them skips the fingerprint check.

### Offline media

- **Opening a project** checks every file (`media.findMissing`). Missing items don't stop the open.
  They render the **offline slate** (`crates/render/src/offline.rs`, our own design: a red striped
  field, a warning triangle, "MEDIA NOT FOUND", the file name and a hint), their audio is silent,
  and the app opens the **Link Media** dialog. `file.open` returns `missingMedia`.
- **Make Offline** (`media.makeOffline {items?, deleteFiles?}`) sets `MediaClip::offline`. The clip
  then shows "MEDIA SET OFFLINE" even though the file exists, until it is linked again. Files are
  deleted only with `deleteFiles: true`.
- The media pool caches sources per item **and** per reference (path, offline flag). So relinking,
  Make Offline and undoing either take effect on the next frame.
- **Stored paths.** Media and proxy paths read from a document (the project file, a project
  template, an interchange file) are opened only when they name a file on this computer or on
  the network share the document itself is on (`crates/engine/src/doc_path.rs`, built on
  `craft-docpath`); a relative path is taken from the document's folder. Other paths (another
  computer, a device) count as missing: the item shows the offline slate, the open says why, and
  the Link Media dialog lists it. A file imported or linked in this session is opened as given.

### Link Media

| Command | |
|---|---|
| `media.findMissing` | `{missing: [{item, name, fileName, path, status: missing\|offline\|proxyMissing, duration, startTimecode}]}` |
| `media.status {item?}` | per item: `online`, `missing`, `offline`, `unreadable` or `generated`, plus identity and proxy |
| `media.linkMedia` | File ▸ Link Media… (rescans; the UI opens the dialog) |
| `media.relink {item, path, force?, relinkOthers?=true, alignTimecode?, match?}` | check, link, then the others |
| `media.autoRelink {from, to}` / `{folder}` | batch relink: a folder-prefix remap, or a search of a folder tree |
| `media.search {folder, item?, exactName?=true}` | ranked candidates with `ok`, `identityMatch`, `problems` |
| `media.replaceFootage {item, path}` | relink without checks (Replace Footage) |
| `media.offlineAll` | close the dialog and leave the rest offline |

`match` selects which properties must agree. Each defaults as shown:

- `fileName` (true): the file name stem;
- `extension` (true);
- `clipId` (true): the fingerprint; a mismatch is refused with "not the same file…";
- `duration` (true): within one frame;
- `mediaStart` (false): the start timecode;
- `metadata` (true): frame size, frame rate and audio channels.

`force: true` links regardless. **Relink others automatically** works out the folder remap from the
file just linked, by dropping the common trailing path components (`/Volumes/A/shoot/a.mov` →
`/Users/me/shoot/a.mov` gives `/Volumes/A` → `/Users/me`). It applies the remap to the other missing
files and checks each one. It also looks in the new file's folder. Windows and macOS separators both
work. **Align Timecode** keeps clips on the same timecode when the new file starts at a different
timecode: the clips' source in-points, keyframes, marks and markers move with it
(`Project::shift_media_time`). Media an EDL names starts at timecode 00:00:00:00 until its file is
linked, because the list's source times are timecodes; relink it with Align Timecode to land on the
same frames as importing the EDL next to its media. One relink, together with the files it pulls
along, is one undo step.

Link Media dialog automation ids: `linkMedia.row.<n>`,
`linkMedia.match.<fileName|extension|clipId|duration|mediaStart|metadata>`, `linkMedia.alignTimecode`,
`linkMedia.relinkOthers`, `linkMedia.folder`, `linkMedia.browse`, `linkMedia.exactName`,
`linkMedia.search`, `linkMedia.candidate.<n>`, `linkMedia.preview`, `linkMedia.link`,
`linkMedia.locate`, `linkMedia.offline`, `linkMedia.offlineAll`, `linkMedia.cancel`.
Make Offline: `makeOffline.keep|delete|ok|cancel`. In the Project panel, offline items get a
broken-link badge (`project.item.<id>.offline` in Icon and List view), and items with a proxy get
a **P** badge (`project.item.<id>.proxy`). Hovering an Unreadable badge shows why the file cannot
be read, such as "no decoder for Apple Intermediate Codec video".

### Proxies

| Command | Menu | |
|---|---|---|
| `media.createProxies {items?, preset?, destination?, attach?=true, wait?}` | Clip ▸ Proxy ▸ Create Proxies… | background job; attaches when done |
| `media.attachProxies {item, path}` / `{items, paths}` | Attach Proxies… | duration within one frame and the same frame rate are required; size and aspect may differ |
| `media.detachProxies {items?}` | Detach Proxies | |
| `media.reconnectFullRes {item, path}` | Reconnect Full Resolution Media… | link the full-resolution file of a clip that has a proxy |
| `media.toggleProxies {enabled?}` | View ▸ Toggle Proxies, monitor button | Preferences ▸ Media ▸ `media.enableProxies` |
| `media.proxyPresets` | | |

The presets use our own encoders:

- `prores_proxy_quarter` (default), `prores_proxy_half`, `prores_lt_half`: ProRes 422 Proxy or LT
  in MOV with PCM audio;
- `h264_quarter`, `h264_half`: H.264 + AAC in MP4.

Proxies go to `<media folder>/Proxies/<name>_Proxy.<ext>`, or to the chosen folder. Job progress is
in `jobs.list`. `Session::poll_persistence`, which the app calls every frame, attaches finished
proxies.

With proxies enabled, monitors, thumbnails and playback read the proxy. The proxy source reports
the original's size. The compositor derives its pixel scale from the frame it gets, so Motion and
pixel-size effect parameters (blur radii…) give the same picture at the proxy's resolution. A
320×180 clip with Motion and Gaussian Blur and a ½-size ProRes Proxy measures 44 dB PSNR against
full resolution (48 dB at ½ playback resolution). **Export always uses full resolution**
(`MediaPool::full_res_provider`). Toggling proxies bumps the revision so caches refresh, but it is
not an edit and doesn't mark the project as modified.

**Cost.** These numbers are from `proxy_playback_perf_4k`, an ignored test: `RAYON_NUM_THREADS=1
cargo test --release -p filmcraft-engine proxy_playback_perf_4k -- --ignored --nocapture`. It plays a
2 s 3840×2160 H.264 clip (ffmpeg `testsrc2`, libx264) on one core of the development Mac and reports
milliseconds per frame:

| | decode (GPU path) | CPU composite at ½ | CPU composite at ¼ | proxy creation |
|---|---|---|---|---|
| full resolution | 9.2 | 277 | 201 | |
| ProRes Proxy ¼ | 8.9 | 150 | 35 | 17.8 s |
| H.264 ¼ | 0.7 | 131 | 48 | 8.9 s |

The test pattern is unusually cheap for H.264 to decode, so real camera footage gains more from
proxies. The CPU composite (YUV→linear conversion and resampling) is what proxies cut most:
5.7× at ¼ resolution, where the proxy needs no resampling.

Create Proxies dialog ids: `proxies.preset.<id>`, `proxies.destination`, `proxies.browse`,
`proxies.ok`, `proxies.cancel`. Monitor button: `program.transport.media.toggleProxies` (and
`source.…`). It is lit while proxies are on.

### Ingest settings

`project.ingestSettings {enabled, action: copy|transcode|createProxies|copyAndCreateProxies,
destination?, preset?}` (Project Settings ▸ Ingest) acts on every `file.import`:

- **copy** copies the file to the destination (default `<media folder>/Ingested Media`), checks the
  copy's fingerprint against the original, and then uses the copy;
- **transcode** writes the preset's format (default ProRes 422 LT) in the background and switches the
  clip to the result when it is done;
- **createProxies** makes proxies in the background and attaches them;
- **copyAndCreateProxies** does both.

Copied files keep their names when available; collisions receive a numbered suffix before the
extension (`A001 2.mov`) and never replace an existing file. Transcodes reserve distinct output
names for every clip in the import batch. Copies keep their source fingerprint, and each proxy
stays attached to its corresponding take.

`file.import` reports this under `ingest`.

## Project Manager

`file.projectManager` (File ▸ Project Manager…) makes a self-contained copy of a project:

```json
{"destination": "/path/Folder", "mode": "collect|consolidate", "sequences": [id], "excludeUnused": true,
 "handles": 30, "preset": "prores_lt", "includeProxies": true, "includePreviews": false,
 "projectName": "…", "dryRun": false, "overwrite": false, "wait": false}
```

- **collect** copies every media file that the chosen sequences use (following nested sequences and
  subclips), plus their proxies and render previews if asked.
- **consolidate** writes only the used range of each movie or audio file, plus `handles` frames on
  each side, with a transcode preset (`prores_lt`, `prores_hq`, `h264`). It then re-bases the clips'
  media time, so every edit, keyframe and marker stays in place. Stills are copied.
- **excludeUnused** drops media items and sequences that the chosen sequences don't use.
- **dryRun** returns the plan and the disk-space estimate (`originalBytes`, `resultBytes`, `files`)
  without writing anything.

The copy runs as a job. The project file (`<destination>/<name>.fcproj`) is written last, with the
new paths and fingerprints, so it opens with no missing media.

Dialog ids: `pm.seq.<id>`, `pm.mode.<collect|consolidate>`, `pm.preset.<id>`, `pm.excludeUnused`,
`pm.handles`, `pm.includeProxies`, `pm.includePreviews`, `pm.destination`, `pm.browse`,
`pm.calculate`, `pm.sizes`, `pm.ok`, `pm.cancel`.

## Effect presets

Effect presets (Effects panel ▸ Presets; Effect Controls ▸ right-click an effect ▸ Save Preset…) are
not part of the project. Built-in presets are defined in code (`filmcraft_engine::presets`); user
presets live in `<data dir>/effect-presets.json`, and `presets.export` / `presets.import` read and
write the same JSON format:

```json
{"format": "filmcraft.effect-presets", "version": 1, "presets": [
  {"name": "My Vignette", "description": "", "keyframes": "AnchorToIn",
   "source_duration": 2540160000000, "source_size": [1920, 1080],
   "effects": [ /* EffectInstance, exactly as in .fcproj: effect, enabled, params, masks */ ]}
]}
```

Keyframe times are relative to the in point of the clip the preset was saved from
(`source_duration` is that clip's media length in ticks). Applying re-times them: `Scale` stretches
them over the target clip, `AnchorToIn` / `AnchorToOut` keep their distance from the target's in /
out point. Point parameters, mask paths, feather and expansion are scaled by the target / source
frame-size ratio. Intrinsic effects (Motion, Opacity…) replace the clip's own instance; other
effects are added. Applying is one undo step; saving, renaming, deleting and importing change the
library, not the project, and are not undoable. Files with another `format`, a newer `version` or
unknown effect ids are refused.

## Marker web addresses (schema v17)

Markers may store an optional `url` independently of their name and notes. Older files
load with an empty URL. Schema v17 prevents older builds from silently dropping links
on save. Changing marker type retains the address. Project loading never opens it; the
Edit Marker dialog offers explicit HTTP(S) browser opening for Web Link markers.

### Copy items from a saved bin

In the Project panel menu, choose **Project History**, select a saved revision and open a bin. This includes bins deleted after that save. Right-click a current bin and choose **Bin History** to follow that bin across renames. The historical window shows saved membership, timing and media settings. Source rows show the current name, label and descriptive columns for that source ID, including cleared custom fields. A source that now exists only in history shows its saved values and a **Saved metadata** indicator. **Show Reference Clips** reveals the sources used by its cuts alongside explicitly filed rows.

Select items, choose **Copy Selected Items**, open a writable destination bin and choose **Paste from Bin History** in its menu. Each selected sequence receives a fresh ID. An unchanged source can be shared. When its historical metadata differs, paste creates a separate source linked to the same media. Pasting another cut from the same revision can reuse that recovered source, including after reopening, while an edit to the recovered source preserves it as an independent version. Copying a reference row explicitly files it in the destination. Sources brought along only by a cut remain reference rows.

Each paste is one Undo step. Save publishes it. The automation equivalents are `project.binHistory` for revision, bin and row listings and `project.pasteFromHistory` for pasting selected item IDs into a writable bin in the same document session. Pass the listing’s `epoch` together with the saved `revision`, destination `bin`, and selected `items`.

#### Compare versions of a bin

In **Bin History**, choose **Compare Versions** beside a save, or open a saved bin in **Project History** and choose **Compare Versions**. The first side defaults to the save that introduced the bin’s preceding distinct state. Either side can be changed. While a comparison loads, you can go back, close the window or choose another save once the revision list is available. Copy becomes available when the selected comparison finishes. Names follow the same bin ID across renames. A source logging edit counts toward the history of every bin that explicitly filed it at that save. Cuts that only referenced that source skip the logging-only save, even with reference rows visible.

Expand **Changed fields** to see the saved values on each side. **Inspect sequence** shows clip edits with their track, timing and source. A damaged record is identified in an incomplete comparison. If damage interrupts automatic lookup, select two revisions manually. Use **Copy from first save** or **Copy from second save**, then **Paste from Bin History** in a writable bin. Copying while inspecting a clip recovers its owning sequence. Browsing, comparing and copying leave the live edit and its Undo history intact.

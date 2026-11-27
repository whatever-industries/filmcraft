# Project files

FilmCraft projects are `.fcproj` files: UTF-8 JSON with an explicit schema version. Reading and
writing them is `crates/format` (`filmcraft-format`); the engine's `file.*` commands use it.

## Format

```json
{
  "format": "filmcraft.project",
  "schema_version": 19,
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
| 18 | Script items and camera RAW | Script builds stored screenplay rows, linked takes, marks, and thumbnail positions. Camera RAW builds stored per-item settings at `interpret.raw`. |
| 19 | Script and RAW compatibility | Reads both v18 shapes and retains Script items and camera RAW settings on save. Existing projects need no data rewrite. |

FilmCraft preserves saved camera RAW white balance, exposure, highlight handling, demosaic quality,
and output color space as project data. Support for applying or editing those settings is separate.

### Migrations

Older files are upgraded on load, in memory, by a chain of single-step functions in the shared format
library (`filmcraft_format::MIGRATIONS`, where entry *i* upgrades v*i+1* to v*i+2*). FilmCraft can
also read and write its own current schema when the shared migrations cover all preceding versions
and the app version needs no structural migration. To change the schema:

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
stay where they are. When a Windows network share and the destination are on different
volumes, the converter can retain an absolute UNC path. FilmCraft still applies its network
access rules when it opens media. Moving a completed folder later can change what its relative paths
resolve to, just as moving a standard project can.

Packing reserves a fresh directory, reads the records back, reconstructs the project with
FilmCraft's standard reader, and publishes the manifest last. An interruption can leave
records in the new directory. A folder without a manifest is incomplete. Retry packing
the original project into a fresh destination. Unpacking checks every referenced digest
and record ID before publishing a new standard file. Both commands refuse occupied
destinations and keep their inputs intact.

Both supported folder layouts accept at most 100,000 bin and item records, a 16 MiB manifest,
and 256 MiB of combined record data. A standard input or export must also fit FilmCraft's
ordinary project size limit.

### Incremental saves

`project_folder checkout FOLDER NEW_FILE.fcproj` exports the current snapshot and prints
its revision digest. Edit the exported file, then run
`project_folder save FOLDER BASE_REVISION EDITED.fcproj` with that digest. A successful
save prints the next digest. An unchanged save returns the same digest. A stale or busy
writer gets an error and retains its edited file. A stale writer can check out a fresh
snapshot before reapplying its edits.

On a local filesystem the writer holds an OS lock across the revision check and commit.
The lock file stays in the folder so later writers lock the same file.
Changed records and the previous manifest are written first. Replacing the current manifest
commits the entire edit, including moves between bins. Changed bin and item records acquire
an internal UTC edit time and revision number. Imported edit times start unknown.
The first changed save upgrades the folder to layout version 2, which retains its previous
manifests by digest. Each save still reads and validates the complete project. Shared
filesystem locking requires separate qualification.

### Save from the desktop editor

Open the folder's `project.fcmod` with **File → Open Project**. The usual **Save**
command publishes only changed bin, item, and project metadata records. Clean records
keep their existing digests. A stale revision or a failed write leaves the editor's
changes unsaved. Reopen the folder to load the latest published revision.

The desktop checkpoints unsaved modular edits into a session-owned recovery store. Dated auto-saves remain standalone project files. The preference that
also overwrites the current project applies to standard `.fcproj` files. Recovered
folder drafts open as unsaved projects, so they can be saved separately. **Save a
Copy** exports a standard project while keeping the folder open.

For an open local folder, the engine also accepts three bin commands.

| Command | Result |
| --- | --- |
| `project.binStatus {}` | Lists `dirtyBins` by ID and reports `projectSettingsDirty` and `unfiledItemsDirty`. |
| `project.saveBin {"bin": id}` | Publishes that bin’s name, membership, directly owned items, timelines, and attached metadata. Saving an unchanged bin writes zero project-folder bytes. |
| `project.discardBin {"bin": id}` | Restores that bin from the published revision. The revision stays the same. Independent edits remain available through Undo and Redo. |

Project settings and notes wait for global Save. Saving a parent preserves edits in its child bins for their own save. If a moved child bin, moved media item, or missing source prevents a bin decision from being applied independently, the command reports the conflict. With crash recovery enabled, the editor confirms the remaining draft before acknowledging the decision. A recovery-cleanup failure after publication identifies the published revision, so retrying Save Bin can complete cleanup.

An open bin appears once per editor session. Moving between bins leaves previous bins open. Closing a bin marked **Unsaved** offers **Save** and **Discard** for that bin. Once the choice succeeds, its directly owned timelines close. Other open sequences can continue playing sources from the closed bin. A bin must have an open view to receive a drop, and a background tab counts.

Move Sequence 1 from Bin A to Bin B and save Bin B on close. The arrival takes the original ID so master timelines follow the saved cut. Bin A keeps its original record until you save its removal. An interrupted session reopens with the older cut as a stable independent copy in Bin A. The copy can initially share an immutable sequence payload. Saving an edit to the copy writes its own record.

If you save Bin A first, the removal protects both the last explicit save and the cut as it left Bin A. Discarding Bin B then leaves Sequence 1 outside both bins. Master timelines still use the last explicitly saved cut. Discarding Bin B before saving Bin A leaves Bin A’s removal Unsaved. [Modular project rules and acceptance tests](modular-project-rules.md)

### Bin backup retention

In a local modular project, open **Project Settings → Recovery** to set **Bin backups per project** and **Versions per bin**, or turn off automatic pruning. Both counts default to 2,000. **Preview cleanup** shows how many saved versions the current choices would retire before you apply them. Saving three edited bins together adds three backups. A successful close-prompt Save adds one. Settings-only saves, background checkpoints, failed publications and clean saves add zero.

When a bin save takes the project above its limit, cleanup keeps the newest eligible versions up to 90% of the project limit, rounded up, and applies the per-bin limit. A version saved across several bins stays together. Its newest backup can exceed the project count on its own. Older folder revisions created before the editor recorded bin counts share a separate history allowance with settings-only versions. New saves start the counter for those folders.

The live project, explicitly kept versions, the last saved source of a moved sequence, and its departure cut remain recoverable. The project also retains the manifest chain and records shared with any surviving version. The cleanup catalog is part of the project folder. With automatic pruning enabled, the desktop resumes outstanding cleanup on the next editor open or changed save. Crash-recovery drafts and dated auto-saves have their own storage.

The editor returns from Save after publishing the edits. A background worker verifies the project history and retired records. Before publishing the cleanup catalog or deleting a batch of records, it checks that the saved revision, pins and policy still match. A newer save or changed policy causes the worker to reconsider the remaining work. Cleanup errors are reported separately from the saved edit. The one-shot project-folder save utility completes its cleanup before exiting.

| Local editor command | Action |
| --- | --- |
| `project.recoveryRetention {}` | Read the current policy. |
| `project.setRecoveryRetention {"enabled":true,"maxBinBackups":2000,"versionsPerBin":2000}` | Set this folder’s policy. In-flight cleanup checks changes before its next commit. Opening the folder or publishing an edit schedules any cleanup due. |
| `project.previewRecoveryPrune {}` | List the revisions a cleanup would retire or keep and the candidate record count. |
| `project.waitForRecoveryPrune {}` | Wait for cleanup already scheduled by this editor and report its result. |
| `project.keepRecoveryVersion {"revision":"…","keep":true}` | Protect a saved version. Set `keep` to false to return it to normal rotation. |

### Recover a sequence

Run `project_folder history FOLDER` to list revision numbers and digests, newest first.
Run `project_folder move-backups FOLDER` to list each recorded publication with its revision number, source bin ID, destination bin ID, sequence ID, last saved source digest, and departure snapshot digest. Use the last digest to recover the exact cut as it left the source bin.

Use `project_folder restore-copy FOLDER BASE_REVISION HISTORICAL_REVISION SEQUENCE_ID`
to add the old cut alongside the current one. The result prints the new project item ID
and revision. The copy goes into the current cut's bin, or the root if the original
was removed. Existing media and nested sequences keep their IDs. If a required source
is missing or changed, its attached data changed, or the project LUT library differs,
use `rescue` to export the historical cut as a separate project.

`project_folder rescue FOLDER HISTORICAL_REVISION SEQUENCE_ID NEW_FILE.fcproj` writes
a standard project with the selected cut, its transitive sources and a fresh root bin.
It follows subclips, nested sequences, multicam angles and merged sources. File-backed
media and proxy references are rebased to the destination. The root manifest, requested
history chain, envelope and required item payloads must still be readable. Unrelated bins
and items can be missing. This command leaves the damaged folder available for further
recovery. Keep a separate backup for loss of the underlying volume.

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

- While edits are unsaved, the worker schedules a checkpoint about one second after the last edit, or at the configured recovery interval during continuous editing. Standard projects write `snapshot.fcproj` and `session.json` in their recovery session directory.
- A modular draft stores bin, item and settings records in a private data file. The first checkpoint captures the full working project. Later checkpoints write changed records into free space, sync the data and index, then atomically replace `session.json` with the checkpoint digest, revision, time and original project path. Only after publication can the next edit reuse the previous cut’s space. A clean interval writes nothing. Checkpoints leave bins Unsaved and add zero to the bin-backup count.
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
- Standard recovery opens an **unsaved** project at its original path. A modular draft opens detached and can be saved separately. Its media and proxy links are resolved against the original project location, even when the project folder is offline. The recovered state goes into the new session's own journal before the old
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
(`cargo test --release -p filmcraft-format --test it fixtures::large_project_roundtrip_and_timing -- --nocapture`):

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
| `media.relink {item, path, force?, confirmScriptSameFootage?, relinkOthers?=true, alignTimecode?, match?}` | check, link, then the others |
| `media.autoRelink {from, to}` / `{folder}` | batch relink: a folder-prefix remap, or a search of a folder tree |
| `media.search {folder, item?, exactName?=true}` | ranked candidates with `ok`, `identityMatch`, `problems` |
| `media.replaceFootage {item, path}` | force relink (Replace Footage); still checks Script marks |
| `media.offlineAll` | close the dialog and leave the rest offline |

`match` selects which properties must agree. Each defaults as shown:

- `fileName` (true): the file name stem;
- `extension` (true);
- `clipId` (true): the fingerprint; a mismatch is refused with "not the same file…";
- `duration` (true): within one frame;
- `mediaStart` (false): the start timecode;
- `metadata` (true): frame size, frame rate and audio channels.

`force: true` bypasses the media matching checks, but not the Script mark review described below.
**Relink others automatically** works out the folder remap from the
file just linked, by dropping the common trailing path components (`/Volumes/A/shoot/a.mov` →
`/Users/me/shoot/a.mov` gives `/Volumes/A` → `/Users/me`). It applies the remap to the other missing
files and checks each one. It also looks in the new file's folder. Windows and macOS separators both
work. **Align Timecode** keeps clips on the same timecode when the new file starts at a different
timecode: the clips' source in-points, keyframes, marks and markers move with it
(`Project::shift_media_time`). Media an EDL names starts at timecode 00:00:00:00 until its file is
linked, because the list's source times are timecodes; relink it with Align Timecode to land on the
same frames as importing the EDL next to its media. One relink, together with the files it pulls
along, is one undo step.

Script takes refer to the same Project media items as sequence clips. Relinking an offline item to
its original file preserves Script marks and candles. If a relink candidate cannot be verified as
the same file and a linked Script take has marks, the command stops before changing any media.
The Link Media dialog asks for an explicit same-footage confirmation; command clients may pass
`confirmScriptSameFootage: true` after review. This preserves the marks; to use different footage,
replace the take in the Script window instead.

Link Media dialog automation ids: `linkMedia.row.<n>`,
`linkMedia.match.<fileName|extension|clipId|duration|mediaStart|metadata>`, `linkMedia.alignTimecode`,
`linkMedia.relinkOthers`, `linkMedia.folder`, `linkMedia.browse`, `linkMedia.exactName`,
`linkMedia.search`, `linkMedia.candidate.<n>`, `linkMedia.preview`, `linkMedia.link`,
`linkMedia.locate`, `linkMedia.offline`, `linkMedia.offlineAll`, `linkMedia.cancel`,
`linkMedia.scriptMarks.cancel`, `linkMedia.scriptMarks.confirm`.
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

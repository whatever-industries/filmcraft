# Crash recovery for unsaved project-folder edits

This is the original checkpoint design. The [modular roadmap](modular-roadmap.md) tracks implementation, verification and the next milestone.

A crash after a checkpoint should cost at most the edits made since that checkpoint. On restart, the editor can inspect or recover the draft. Other readers continue to open the last manually saved revision. The first milestone runs through the local CLI on a local filesystem.

Folder editing currently supports manual Save, Undo, Redo and standard export. The single-file journal already schedules background writes, discovers abandoned sessions and acknowledges flushes. Its worker serializes a typed project, which can omit extension fields. Folder recovery needs the same lossless merge as manual folder export, including the historical baseline restored by Undo.

## 1. Write a durable recovery checkpoint

- Start with a self-contained project metadata snapshot in the existing local recovery directory. Reuse the folder export merge and path handling. Capture the folder identity and path, expected last manual revision, edit generation, view and historical Undo baseline together. Never infer the expected manual revision from that Undo baseline.
- Give the worker an immutable snapshot. It can serialize and write off the editing thread without borrowing the live folder writer. Checkpointing leaves the draft dirty. Only explicit Save can advance the source manifest in this milestone.
- Publish the payload and its metadata as one generation. Write and validate the content first, then atomically replace a small descriptor that identifies it. Acknowledge the checkpoint only after publication. Keep the preceding usable checkpoint until publication succeeds. Validate sizes, digest and ownership on read. An interrupted first write produces no completed checkpoint.
- Use a stable recovery location for relative media and proxy paths. A recovered export must resolve them to the same originals even if the source folder has moved or become unavailable. This copies project metadata only. Measure write time and bytes for a one-clip edit in a large project before deciding whether the journal should reuse individual unchanged records.

## 2. Recover a draft in a fresh process

- Extend the existing candidate-discovery, recover and discard paths for folder drafts. A live writer's journal belongs to that writer. Listing candidates must preserve unrecognized or damaged evidence.
- If the original folder can be identified, its head matches and the writer lease is available, recover into a dirty folder session. Manual Save publishes against that expected head. Otherwise offer recovery to a new, unoccupied standard project file. A missing folder, changed head or busy writer must never result in overwriting another editor's work.
- Preserve the opaque JSON when installing the recovered draft and on its next edit, Save or export. Confirm a replacement checkpoint or a successful explicit save before retiring the only recoverable copy. Undo history after restarting is outside this milestone.
- Exercise the real CLI and a second reader. Move a clip with nested extra fields, checkpoint, terminate the writer and recover. Compare clip and track IDs, timing, effect data, neighboring clips and offline original/proxy references. Repeat with Delete → Save → Undo → checkpoint → crash. Open the exported result in the stock reader.

## 3. Isolate Save, project switching and failures

- Associate each queued write, completion and cleanup with a document session and edit generation. A late completion for project A must never clear, replace or mark saved a draft for B. Mark only the generation covered by a successful manual Save as clean. A newer pending edit stays recoverable.
- Before releasing a dirty document on switch, confirm its pending checkpoint or obtain an explicit discard through the existing lifecycle. A failed switch keeps the open project and its writer lease usable. Cover folder-to-folder and folder-to-standard transitions, plus two identically named projects and a read-only peer.
- Use deterministic barriers around payload write, descriptor publication and worker acknowledgement. Terminate a real subprocess between writes. Verify that the previous checkpoint opens, repeated recovery is safe, and failed writes can be retried. Test a full disk or injected I/O error, a bad digest, a missing payload and a stale completion after switch.

## Delivery

Make each slice a tested local commit on the modular fork. Run the focused engine, folder-format, Undo and single-file recovery regressions throughout. Finish with the full workspace gate, compare any machine-specific failures with the recorded baseline, and record a real crash/reopen run. Editor recovery controls, periodic publication to the shared project and network filesystem behavior are separate milestones.

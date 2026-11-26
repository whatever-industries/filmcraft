//! Compare clip identity across all tracks. Retain unrecognized fields for fallback reporting.
use super::without;
use crate::project_folder::invalid;
use filmcraft_project::SequenceSettings;
use filmcraft_time::{Tick, format_timecode_frames};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
};

struct Clip {
    track: u64,
    detail: Value,
    extra: Value,
}
#[derive(Default)]
pub(super) struct Cut {
    clips: BTreeMap<u64, Clip>,
    order: BTreeMap<u64, Vec<u64>>,
    other: Value,
}
impl Cut {
    pub(super) fn read(item: &Value) -> io::Result<Self> {
        let mut other = without(item, &["name"]);
        let seq = other.pointer_mut("/kind/Sequence").ok_or_else(|| invalid("expected a sequence"))?;
        let settings: SequenceSettings = serde::Deserialize::deserialize(&seq["settings"])?;
        settings.validate().map_err(invalid)?;
        let rate = settings.frame_rate;
        let tc = |frames| format_timecode_frames(frames, rate, settings.drop_frame);
        let origin = seq["start_timecode"].as_i64().ok_or_else(|| invalid("missing sequence start timecode"))?;
        let mut clips = BTreeMap::new();
        let mut order = BTreeMap::new();
        for group in ["video_tracks", "audio_tracks", "submix_tracks"] {
            let Some(tracks) = seq.get_mut(group).and_then(Value::as_array_mut) else { continue };
            for track in tracks {
                let track_id = track["id"].as_u64().ok_or_else(|| invalid("missing track ID"))?;
                let mut ids = Vec::new();
                let entries = track["items"].as_array_mut().ok_or_else(|| invalid("invalid track items"))?;
                for raw in entries.drain(..) {
                    let id = raw["id"].as_u64().ok_or_else(|| invalid("missing clip ID"))?;
                    let ticks = |field: &str| raw[field].as_i64().ok_or_else(|| invalid(format!("clip {id}: invalid {field}")));
                    let start = ticks("start")?;
                    let duration = ticks("duration")?;
                    let source = ticks("source_in")?;
                    let start_frame = rate.frame_at(Tick(start)).checked_add(origin).ok_or_else(|| invalid(format!("clip {id}: timecode overflow")))?;
                    let detail = json!({"track":track_id,"source":raw["item"],"start":start,"duration":duration,"sourceIn":source,
                        "startTimecode":tc(start_frame),"durationTimecode":tc(rate.frame_at(Tick(duration))),"sourceInAtSequenceRate":tc(rate.frame_at(Tick(source)))});
                    let extra = without(&raw, &["id", "item", "start", "duration", "source_in"]);
                    if clips.insert(id, Clip { track: track_id, detail, extra }).is_some() {
                        return Err(invalid(format!("duplicate clip {id}")));
                    }
                    ids.push(id);
                }
                if order.insert(track_id, ids).is_some() {
                    return Err(invalid(format!("duplicate track {track_id}")));
                }
            }
        }
        Ok(Self { clips, order, other })
    }
}
pub(super) fn compare(id: u64, a: &Cut, b: &Cut, rows: &mut Vec<Value>) {
    let mut other = a.other != b.other;
    for (track, before) in &a.order {
        if let Some(after) = b.order.get(track) {
            let stays = |id: &&u64| a.clips.get(id).zip(b.clips.get(id)).is_some_and(|(x, y)| x.track == *track && y.track == *track);
            other |= before.iter().filter(stays).ne(after.iter().filter(stays));
        }
    }
    if other {
        rows.push(json!({"sequence":id,"change":"other"}));
    }
    for &clip in a.clips.keys().chain(b.clips.keys()).collect::<BTreeSet<_>>() {
        let (old, new) = (a.clips.get(&clip), b.clips.get(&clip));
        let kinds = match (old, new) {
            (Some(x), Some(y)) => {
                let mut kinds = Vec::new();
                for (field, change) in [
                    ("track", "trackMoved"),
                    ("start", "timelineMoved"),
                    ("duration", "trimmed"),
                    ("source", "sourceReplaced"),
                    ("sourceIn", "sourceInChanged"),
                ] {
                    if x.detail[field] != y.detail[field] {
                        kinds.push(change);
                    }
                }
                if x.extra != y.extra {
                    kinds.push("other");
                }
                kinds
            }
            (None, Some(_)) => vec!["added"],
            (Some(_), None) => vec!["removed"],
            (None, None) => continue,
        };
        if !kinds.is_empty() {
            rows.push(json!({"sequence":id,"clip":clip,"changes":kinds,"before":old.map(|c|&c.detail),"after":new.map(|c|&c.detail)}));
        }
    }
}

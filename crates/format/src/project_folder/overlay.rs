//! Apply typed edits to the saved representation. Each item record supplies its own
//! identity scope: a copied sequence may legitimately repeat another cut's clip IDs.
use super::invalid;
use serde_json::Value;
use std::{collections::BTreeMap, io};

type Pair<'a> = (&'a Value, &'a Value);

pub(super) fn record(raw: &Value, before: &Value, after: &Value, path: &str) -> io::Result<Value> {
    if before == after {
        return Ok(raw.clone());
    }
    if raw == before {
        return Ok(after.clone());
    }
    let mut clips = BTreeMap::new();
    if let (Some(old), Some(stored)) = (before.pointer("/kind/Sequence"), raw.pointer("/kind/Sequence")) {
        for tracks in ["video_tracks", "audio_tracks"] {
            let stored_tracks = by_id(stored[tracks].as_array().map(Vec::as_slice).unwrap_or_default(), "id", path)?;
            for track in old[tracks].as_array().into_iter().flatten() {
                let Some(stored_track) = track["id"].as_u64().and_then(|id| stored_tracks.get(&id)) else { continue };
                let stored_items = by_id(stored_track["items"].as_array().map(Vec::as_slice).unwrap_or_default(), "id", path)?;
                for clip in track["items"].as_array().into_iter().flatten() {
                    if let Some(id) = clip["id"].as_u64()
                        && let Some(&original) = stored_items.get(&id)
                        && clips.insert(id, (original, clip)).is_some()
                    {
                        return Err(invalid(format!("{path}: ambiguous clip id {id}")));
                    }
                }
            }
        }
    }
    merge(Some((raw, before)), after, path, "", &clips)
}

fn by_id<'a>(array: &'a [Value], key: &str, path: &str) -> io::Result<BTreeMap<u64, &'a Value>> {
    let mut lookup = BTreeMap::new();
    for v in array {
        let id = v[key].as_u64().ok_or_else(|| invalid(format!("{path}: missing {key}")))?;
        if lookup.insert(id, v).is_some() {
            return Err(invalid(format!("{path}: duplicate {key} {id}")));
        }
    }
    Ok(lookup)
}

// Missing serialized defaults are common. Only data present in the original and
// absent from the typed snapshot needs an owner when an unkeyed collection changes.
fn extra(raw: &Value, typed: &Value) -> bool {
    match (raw, typed) {
        (Value::Object(a), Value::Object(b)) => a.iter().any(|(k, v)| b.get(k).is_none_or(|known| extra(v, known))),
        (Value::Array(a), Value::Array(b)) => a.len() != b.len() || a.iter().zip(b).any(|(r, t)| extra(r, t)),
        (Value::Object(_) | Value::Array(_), _) => true,
        _ => false,
    }
}

fn merge(pair: Option<Pair<'_>>, next: &Value, path: &str, field: &str, clips: &BTreeMap<u64, Pair<'_>>) -> io::Result<Value> {
    if let Some((raw, old)) = pair
        && old == next
    {
        return Ok(raw.clone());
    }
    match next {
        Value::Object(obj) => {
            let mut result = pair.and_then(|(r, _)| r.as_object()).cloned().unwrap_or_default();
            if let Some(old) = pair.and_then(|(_, t)| t.as_object()) {
                result.retain(|k, _| !old.contains_key(k) || obj.contains_key(k));
            }
            for (k, v) in obj {
                let old = pair.and_then(|(_, t)| t.get(k));
                if old == Some(v) {
                    continue;
                }
                let original = pair.and_then(|(r, _)| r.get(k));
                result.insert(k.clone(), merge(original.zip(old), v, &format!("{path}/{k}"), k, clips)?);
            }
            Ok(Value::Object(result))
        }
        Value::Array(array) => {
            let (raw, old) = pair
                .map(|(r, t)| (r.as_array().map(Vec::as_slice).unwrap_or_default(), t.as_array().map(Vec::as_slice).unwrap_or_default()))
                .unwrap_or_default();
            let key = match field {
                "items" if path.contains("/kind/Sequence/") => Some("id"),
                "video_tracks" | "audio_tracks" | "caption_tracks" | "submix_tracks" | "markers" | "transitions" | "captions" => Some("id"),
                _ => None,
            };
            if let Some(k) = key {
                let previous = by_id(old, k, path)?;
                let originals = by_id(raw, k, path)?;
                by_id(array, k, path)?;
                return array
                    .iter()
                    .map(|v| {
                        let id = v[k].as_u64().ok_or_else(|| invalid(format!("{path}: missing {k}")))?;
                        let pair = originals
                            .get(&id)
                            .copied()
                            .zip(previous.get(&id).copied())
                            .or_else(|| (field == "items").then(|| clips.get(&id).copied()).flatten());
                        merge(pair, v, &format!("{path}/{id}"), "", clips)
                    })
                    .collect::<io::Result<Vec<_>>>()
                    .map(Value::Array);
            }
            if pair.is_some_and(|(r, t)| extra(r, t)) {
                if array.is_empty() {
                    return Ok(next.clone());
                }
                // A graphic layer has a stable uid. Ordinary effects only identify their
                // type, which is sufficient when that type occurs once on this owner.
                if old.iter().chain(array).all(|v| v["effect"].is_string()) && raw.len() == old.len() {
                    return effects(raw, old, array, path, clips);
                }
                return Err(invalid(format!("{path}: cannot identify owners of extra fields after editing this collection")));
            }
            Ok(next.clone())
        }
        _ => Ok(next.clone()),
    }
}

fn effects(raw: &[Value], old: &[Value], new: &[Value], path: &str, clips: &BTreeMap<u64, Pair<'_>>) -> io::Result<Value> {
    #[derive(Eq, PartialEq, Ord, PartialOrd)]
    enum Key<'a> {
        Layer(u64),
        Effect(&'a str),
    }
    fn key(v: &Value) -> Key<'_> {
        match v.pointer("/layer/uid").and_then(Value::as_u64).filter(|id| *id != 0) {
            Some(id) => Key::Layer(id),
            None => Key::Effect(v["effect"].as_str().unwrap_or_default()),
        }
    }
    let mut previous = BTreeMap::<_, Vec<usize>>::new();
    for (i, v) in old.iter().enumerate() {
        previous.entry(key(v)).or_default().push(i);
    }
    let mut counts = BTreeMap::new();
    for v in new {
        *counts.entry(key(v)).or_insert(0usize) += 1;
    }
    let mut result = Vec::with_capacity(new.len());
    for v in new {
        let k = key(v);
        match previous.get(&k).map(Vec::as_slice) {
            Some([i]) if counts.get(&k) == Some(&1) => result.push(merge(Some((&raw[*i], &old[*i])), v, path, "", clips)?),
            Some(indices) if indices.iter().any(|&i| extra(&raw[i], &old[i])) => {
                return Err(invalid(format!("{path}: ambiguous effect instance owns extra fields")));
            }
            _ => result.push(v.clone()),
        }
    }
    Ok(Value::Array(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn same_clip_moving_into_a_new_track_keeps_its_extensions() {
        let old = json!({"kind":{"Sequence":{"video_tracks":[{"id":3,"items":[{"id":9,"duration":5}]}]}}});
        let mut raw = old.clone();
        raw["kind"]["Sequence"]["video_tracks"][0]["items"][0]["extra"] = json!({"version":27});
        let new = json!({"kind":{"Sequence":{"video_tracks":[{"id":3,"items":[]},{"id":4,"items":[{"id":9,"duration":2},{"id":10,"duration":2}]}]}}});
        let saved = record(&raw, &old, &new, "item 6").unwrap();
        let clips = &saved["kind"]["Sequence"]["video_tracks"][1]["items"];
        assert_eq!(clips[0], json!({"id":9,"duration":2,"extra":{"version":27}}));
        assert!(clips[1].get("extra").is_none());
    }

    #[test]
    fn ambiguous_effects_refuse_a_save_and_unique_layers_keep_their_own_data() {
        let old = json!({"effects":[{"effect":"blur","enabled":false},{"effect":"blur","enabled":true}]});
        let mut raw = old.clone();
        raw["effects"][0]["extension"] = json!(7);
        let mut next = old.clone();
        next["effects"].as_array_mut().unwrap().reverse();
        let error = record(&raw, &old, &next, "item 6").unwrap_err().to_string();
        assert!(error.contains("item 6/effects") && error.contains("ambiguous"), "{error}");
        let mut old = old;
        for i in 0..2 {
            old["effects"][i]["layer"] = json!({"uid":i+1});
            raw["effects"][i]["layer"] = old["effects"][i]["layer"].clone();
        }
        let mut next = old.clone();
        next["effects"].as_array_mut().unwrap().reverse();
        assert_eq!(record(&raw, &old, &next, "item 6").unwrap()["effects"][1]["extension"], 7);
    }
}

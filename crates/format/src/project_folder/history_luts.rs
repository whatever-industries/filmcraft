//! Keep library LUTs used by a recovered grade, including animated parameter values.
use super::{ProjectRecords, invalid};
use serde_json::Value;
use std::io;

fn values(raw: &mut Value, visit: &mut impl FnMut(&mut Value) -> io::Result<()>) -> io::Result<()> {
    match raw {
        Value::Array(rows) => {
            for row in rows {
                values(row, visit)?;
            }
        }
        Value::Object(obj) => {
            if obj.get("effect").is_some_and(Value::is_string)
                && let Some(params) = obj.get_mut("params")
            {
                for key in ["input_lut", "look_lut"] {
                    if let Some(param) = params.get_mut(key) {
                        if let Some(text) = param.pointer_mut("/value/Text") {
                            visit(text)?;
                        }
                        for frame in param.get_mut("keyframes").and_then(Value::as_array_mut).into_iter().flatten() {
                            if let Some(text) = frame.pointer_mut("/value/Text") {
                                visit(text)?;
                            }
                        }
                    }
                }
            }
            for value in obj.values_mut() {
                values(value, visit)?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(super) fn referenced(raw: &mut Value, ids: &mut std::collections::BTreeSet<String>) -> io::Result<()> {
    values(raw, &mut |text| {
        if let Some(id) = text.as_str().and_then(|s| s.strip_prefix("lib:")) {
            ids.insert(id.to_owned());
        }
        Ok(())
    })
}

pub(super) fn restore(raw: &mut Value, old: &ProjectRecords, live: &mut ProjectRecords, next: &mut u64) -> io::Result<()> {
    values(raw, &mut |text| {
        let Some(old_id) = text.as_str().and_then(|v| v.strip_prefix("lib:")) else {
            return Ok(());
        };
        let lut = old.envelope["project"]["luts"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|l| l["id"] == old_id)
            .ok_or_else(|| invalid(format!("missing historical LUT {old_id}")))?;
        let project = live.envelope["project"].as_object_mut().ok_or_else(|| invalid("expected project metadata"))?;
        let library = project.entry("luts").or_insert_with(|| Value::Array(Vec::new())).as_array_mut().ok_or_else(|| invalid("invalid LUT library"))?;
        let id = if let Some(id) = library.iter().find(|l| l["format"] == lut["format"] && l["text"] == lut["text"]).and_then(|l| l["id"].as_str()) {
            id.to_owned()
        } else {
            let id = format!("lut{}", super::history_paste::allocate(next)?);
            if library.iter().any(|l| l["id"] == id) {
                return Err(invalid("recovered LUT ID already exists"));
            }
            let mut copy = lut.clone();
            copy["id"] = id.clone().into();
            library.push(copy);
            id
        };
        *text = format!("lib:{id}").into();
        Ok(())
    })
}

pub(super) fn verify_remaining(records: &ProjectRecords, removed: &std::collections::BTreeSet<String>) -> io::Result<()> {
    if removed.is_empty() {
        return Ok(());
    }
    for raw in records.items.values().chain(records.envelope["project"].get("source_graphics")) {
        values(&mut raw.clone(), &mut |text| {
            if text.as_str().and_then(|s| s.strip_prefix("lib:")).is_some_and(|id| removed.contains(id)) {
                return Err(invalid("another edit still uses the recovered LUT"));
            }
            Ok(())
        })?;
    }
    Ok(())
}

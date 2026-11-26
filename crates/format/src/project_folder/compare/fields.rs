//! Bounded field details for a changed record, including fields from newer schemas.
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub(super) fn details(before: Option<&Value>, after: Option<&Value>) -> Value {
    let mut rows = Vec::new();
    walk("", before, after, &mut rows);
    json!(rows)
}

fn display(v: &Value) -> Value {
    match v {
        Value::String(s) if s.len() > 240 => Value::String(s.chars().take(240).chain(['…']).collect()),
        Value::Array(a) => json!({"entries":a.len()}),
        Value::Object(o) => json!({"fields":o.len()}),
        _ => v.clone(),
    }
}

fn walk(path: &str, a: Option<&Value>, b: Option<&Value>, out: &mut Vec<Value>) {
    const LIMIT: usize = 128;
    if a == b || out.len() > LIMIT {
        return;
    }
    if out.len() == LIMIT {
        out.push(json!({"more":true}));
        return;
    }
    if a.is_none_or(Value::is_object) && b.is_none_or(Value::is_object) {
        let keys: BTreeSet<_> = a.into_iter().chain(b).filter_map(Value::as_object).flat_map(|o| o.keys()).collect();
        if !keys.is_empty() {
            for key in keys {
                let escaped = key.replace('~', "~0").replace('/', "~1");
                walk(&format!("{path}/{escaped}"), a.and_then(|v| v.get(key)), b.and_then(|v| v.get(key)), out);
                if out.len() > LIMIT {
                    break;
                }
            }
            return;
        }
    }
    if let (Some(left), Some(right)) = (a.and_then(Value::as_array), b.and_then(Value::as_array)) {
        for n in 0..left.len().max(right.len()) {
            walk(&format!("{path}/{n}"), left.get(n), right.get(n), out);
            if out.len() > LIMIT {
                break;
            }
        }
        return;
    }
    let mut row = json!({"path":path});
    if let Some(v) = a {
        row["before"] = display(v);
    }
    if let Some(v) = b {
        row["after"] = display(v);
    }
    out.push(row);
}

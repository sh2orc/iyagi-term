//! Bounded file-change evidence, correlated with the exact provider item.
use serde_json::{json, Value};
use std::collections::HashMap;

const ITEM_BYTES: usize = 64 * 1024;
const CACHE_BYTES: usize = 256 * 1024;
const CACHE_ITEMS: usize = 32;
type Key = (String, String, String);

#[derive(Default)]
pub(super) struct FileChanges {
    items: HashMap<Key, (Value, usize)>,
    bytes: usize,
}

fn key(params: &Value, item: Option<&str>) -> Option<Key> {
    let thread = params["threadId"].as_str()?;
    let turn = params["turnId"].as_str()?;
    let item = item.or_else(|| params["itemId"].as_str())?;
    if [thread, turn, item]
        .iter()
        .any(|id| id.is_empty() || id.len() > 256)
    {
        return None;
    }
    Some((thread.into(), turn.into(), item.into()))
}

impl FileChanges {
    pub fn observe(&mut self, params: &Value, item: Option<&Value>) {
        let Some(key) = key(params, item.and_then(|item| item["id"].as_str())) else {
            return;
        };
        self.remove_key(&key);
        let changes = item.unwrap_or(params).get("changes");
        let Some(changes) = changes.and_then(Value::as_array) else {
            return;
        };
        if changes.is_empty()
            || changes.len() > 64
            || !changes.iter().all(|change| {
                change["path"].is_string()
                    && change["diff"].is_string()
                    && matches!(
                        change["kind"]["type"].as_str(),
                        Some("add" | "delete" | "update")
                    )
            })
        {
            return;
        }
        // Retain only the schema fields that describe the requested changes.
        let changes = Value::Array(
            changes
                .iter()
                .map(|change| {
                    json!({
                        "path":change["path"], "kind":change["kind"], "diff":change["diff"]
                    })
                })
                .collect(),
        );
        let bytes = serde_json::to_vec(&changes).expect("JSON value").len();
        if bytes > ITEM_BYTES || self.bytes + bytes > CACHE_BYTES || self.items.len() >= CACHE_ITEMS
        {
            return;
        }
        self.bytes += bytes;
        self.items.insert(key, (changes, bytes));
    }

    fn remove_key(&mut self, key: &Key) {
        if let Some((_, bytes)) = self.items.remove(key) {
            self.bytes -= bytes;
        }
    }

    pub fn completed(&mut self, params: &Value) {
        if let Some(key) = key(params, params["item"]["id"].as_str()) {
            self.remove_key(&key);
        }
    }

    pub fn question(&self, params: &Value) -> String {
        let changes = key(params, None)
            .and_then(|key| self.items.get(&key))
            .map(|(value, _)| value);
        serde_json::to_string_pretty(&json!({
            "type":"file_change", "reason":params["reason"], "grant_root":params["grantRoot"],
            "details_available":changes.is_some(), "changes":changes,
        }))
        .expect("JSON value")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn started() -> Value {
        json!({"threadId":"thread", "turnId":"turn", "item":{"id":"item", "changes":[{"path":"answer.txt", "kind":{"type":"add"}, "diff":"+42\n"}]}})
    }
    fn request() -> Value {
        json!({"threadId":"thread", "turnId":"turn", "itemId":"item", "reason":"Create requested file", "grantRoot":null})
    }
    #[test]
    fn approval_uses_only_matching_live_item_and_preserves_the_root_grant() {
        let mut cache = FileChanges::default();
        let item = started();
        cache.observe(&item, Some(&item["item"]));
        let request = request();
        let details: Value = serde_json::from_str(&cache.question(&request)).unwrap();
        assert_eq!(details["changes"], item["item"]["changes"]);
        for field in ["threadId", "turnId", "itemId"] {
            let mut wrong = request.clone();
            wrong[field] = json!("other");
            assert!(
                serde_json::from_str::<Value>(&cache.question(&wrong)).unwrap()["changes"]
                    .is_null()
            );
        }
        let mut grant = request.clone();
        grant["grantRoot"] = json!("/outside");
        assert_eq!(
            serde_json::from_str::<Value>(&cache.question(&grant)).unwrap()["grant_root"],
            "/outside"
        );
        cache.completed(&item);
        assert_eq!(
            serde_json::from_str::<Value>(&cache.question(&request)).unwrap()["details_available"],
            false
        );
        assert_eq!(cache.bytes, 0);
    }
    #[test]
    fn oversized_or_invalid_update_removes_old_evidence_and_cache_is_bounded() {
        let mut cache = FileChanges::default();
        let item = started();
        cache.observe(&item, Some(&item["item"]));
        let mut update = request();
        update["changes"] = item["item"]["changes"].clone();
        update["changes"][0]["diff"] = json!("x".repeat(ITEM_BYTES));
        cache.observe(&update, None);
        assert_eq!(cache.bytes, 0);
        for number in 0..(CACHE_ITEMS + 1) {
            let mut item = item.clone();
            item["item"]["id"] = json!(format!("item-{number}"));
            cache.observe(&item, Some(&item["item"]));
        }
        assert_eq!(cache.items.len(), CACHE_ITEMS);
        assert!(cache.bytes <= CACHE_BYTES);
        let mut malformed = request();
        malformed["itemId"] = json!("item-0");
        malformed["changes"] = json!([{"path":"bad"}]);
        cache.observe(&malformed, None);
        assert_eq!(cache.items.len(), CACHE_ITEMS - 1);
    }
}

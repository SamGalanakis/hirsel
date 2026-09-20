use super::*;
fn icon_schema() -> Value {
    json!({
        "description":"A vocabulary symbol on a tinted tile, or an image. Image sources are normalized to a 256 px square retained blob; use exactly one of blob_id or artifact_id. Null restores the title monogram; omit to preserve on update.",
        "oneOf":[
            {"type":"null"},
            {"type":"object","additionalProperties":false,"required":["kind","name"],"properties":{"kind":{"const":"symbol"},"name":{"type":"string","enum":hirsel_proto::THREAD_SYMBOLS.as_slice(),"description":"One curated symbol name. Anything else is refused."},"tint":{"type":"string","enum":["neutral","red","orange","amber","green","teal","blue","violet","pink"],"description":"Tile colour; defaults to neutral."}}},
            {"type":"object","additionalProperties":false,"required":["kind","blob_id"],"properties":{"kind":{"const":"image"},"blob_id":{"type":"string","minLength":1}}},
            {"type":"object","additionalProperties":false,"required":["kind","artifact_id"],"properties":{"kind":{"const":"image"},"artifact_id":{"type":"integer","minimum":1,"description":"Accessible file artifact whose content is base64-encoded PNG, JPEG, or WebP bytes."}}}
        ]
    })
}
fn attention_schema() -> Value {
    json!({"type":"string","enum":["quiet","needs_owner"]})
}
pub(super) fn thread_ref_schema() -> Value {
    json!({"oneOf":[{"type":"integer","minimum":1},{"const":"."}],"description":"Self (.), or any Thread ID. Any ID may be named; one outside your reach returns a typed refusal rather than an error."})
}
/// The place a Thread hangs from or a listing stands on: a Thread reference,
/// or `0` — the top of the tree, whose naming takes a root grant.
pub(super) fn thread_place_schema() -> Value {
    json!({"oneOf":[{"type":"integer","minimum":0},{"const":"."}],"description":"Self (.), a Thread ID within reach, or 0 for the top of the tree. Naming 0 takes root reach; anything outside your reach returns a typed refusal rather than an error."})
}
fn grant_target_schema() -> Value {
    json!({"anyOf":[thread_ref_schema(),{"const":"root"}],"description":"A Thread ID or caller-relative path, or \"root\" for every Thread in the history including ones made later. You can only hand on root if you hold it."})
}
pub(super) fn thread_grant_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["thread","target"],"properties":{"thread":thread_ref_schema(),"target":grant_target_schema(),"note":{"type":"string","maxLength":200,"description":"Why this reach exists. Shown to the Owner beside the grant."}}})
}
pub(super) fn thread_revoke_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["thread","target"],"properties":{"thread":thread_ref_schema(),"target":{"anyOf":[{"type":"integer","minimum":1},{"const":"root"}],"description":"The granted target exactly as threads.context lists it: a Thread ID, or \"root\"."}}})
}
pub(super) fn thread_create_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["client_id","kind","title"],"properties":{"client_id":{"type":"string","minLength":1},"kind":{"type":"string","enum":["space","task"]},"title":{"type":"string","minLength":1},"parent":thread_place_schema(),"description":{"type":"string"},"icon":icon_schema(),"instrument":{"type":["object","array","null"],"description":"A nonempty instrument object or array of components; null removes the instrument. Empty objects and arrays are invalid."},"attention":attention_schema()}})
}
pub(super) fn thread_update_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{"thread":thread_ref_schema(),"title":{"type":"string","minLength":1},"description":{"type":"string"},"icon":icon_schema(),"showcased_artifact_id":{"type":["integer","null"],"minimum":1,"description":"Show one accessible artifact beside this Thread chat. Omit to preserve; null removes. Self or descendants only. This explicit reference grants the target scope read access while showcased; it creates no conversation card."},"instrument":{"type":["object","array","null"],"description":"A nonempty instrument object or array of components; null removes the instrument. Empty objects and arrays are invalid."},"attention":attention_schema()}})
}
pub(super) fn thread_state_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["headline"],"properties":{"thread":thread_ref_schema(),"headline":{"type":"string","minLength":1,"maxLength":240,"description":"A normalized, nonempty headline of at most 12 words."}}})
}
pub(super) fn thread_list_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{"under":thread_place_schema(),"depth":{"type":"integer","minimum":1,"maximum":8,"default":1},"limit":{"type":"integer","minimum":1,"maximum":100,"default":50},"after_id":{"type":"integer","minimum":0}}})
}
pub(super) fn thread_read_schema() -> Value {
    let boundary = json!({"type":["integer","null"],"minimum":0});
    json!({"type":"object","additionalProperties":false,"properties":{"thread":thread_ref_schema(),"limit":{"type":"integer","minimum":1,"maximum":100,"default":30},"cursor":{"type":["object","null"],"additionalProperties":false,"required":["messages_before","turns_before","activities_before"],"properties":{"messages_before":boundary,"turns_before":boundary,"activities_before":boundary}}}})
}
pub(super) fn thread_activity_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["kind","data"],"properties":{"thread":thread_ref_schema(),"kind":{"type":"string","minLength":1},"data":{"type":"object"}}})
}
pub(super) fn thread_result_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["thread_id","thread"],"properties":{"thread_id":{"type":"integer","minimum":0},"thread":{"type":"object"},"previous_showcased_artifact_id":{"type":["integer","null"],"description":"Previous artifact reference when a showcase was explicitly changed."},"history_id":{"type":"string"},"reference_url":{"type":"string","description":"Canonical relative URL for ordinary Markdown Thread references."}}})
}
/// An archive reports the whole subtree it moved, not just its root.
pub(super) fn thread_archive_result_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["thread_id","archived","threads","cancelled_turn_ids","activity"],"properties":{"thread_id":{"type":"integer","minimum":0},"archived":{"type":"boolean"},"threads":{"type":"array","items":{"type":"object"}},"cancelled_turn_ids":{"type":"array","items":{"type":"integer","minimum":1}},"activity":{"type":"object"},"history_id":{"type":"string"},"reference_url":{"type":"string","description":"Canonical relative URL for ordinary Markdown Thread references."}}})
}
pub(super) fn thread_send_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["client_id","thread","text"],"properties":{"client_id":{"type":"string","minLength":1},"thread":thread_ref_schema(),"text":{"type":"string","minLength":1},"artifact_ids":{"type":"array","maxItems":100,"items":{"type":"integer","minimum":1}}}})
}
pub(super) fn thread_report_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["summary","artifact_ids"],"properties":{"summary":{"type":"string","minLength":1},"artifact_ids":{"type":"array","maxItems":100,"items":{"type":"integer","minimum":1}}}})
}

pub(super) fn thread_add_related_schema() -> Value {
    let target = json!({"oneOf":[
        {"type":"object","additionalProperties":false,"required":["kind","url"],"properties":{"kind":{"const":"url"},"url":{"type":"string","minLength":1,"maxLength":4096}}},
        {"type":"object","additionalProperties":false,"required":["kind","thread"],"properties":{"kind":{"const":"thread"},"thread":thread_ref_schema()}}
    ]});
    json!({"type":"object","additionalProperties":false,"required":["target"],"properties":{"thread":thread_ref_schema(),"target":target,"title":{"type":"string","maxLength":200}}})
}
pub(super) fn thread_remove_related_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["item_id"],"properties":{"thread":thread_ref_schema(),"item_id":{"type":"integer","minimum":1}}})
}

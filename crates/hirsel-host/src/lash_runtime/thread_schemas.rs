use super::*;
fn icon_schema() -> Value {
    json!({"type":["string","null"],"minLength":1,"maxLength":16,"description":"Compact plain-text emoji or symbol, at most 16 Unicode code points / 64 UTF-8 bytes; no controls or line separators. Null restores the generated avatar; omit to preserve the current icon on update."})
}
fn attention_schema() -> Value {
    json!({"type":"string","enum":["quiet","needs_owner"]})
}
pub(super) fn thread_ref_schema() -> Value {
    json!({"oneOf":[{"type":"integer","minimum":0},{"type":"string","pattern":"^(\\.|\\./[0-9]+(/[0-9]+)*)$"}],"description":"Self (.), a descendant numeric ID, or actual direct-child hops ./id/id. IDs never bypass scope."})
}
pub(super) fn thread_create_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["client_id","title"],"properties":{"client_id":{"type":"string","minLength":1},"title":{"type":"string","minLength":1},"parent":thread_ref_schema(),"description":{"type":"string"},"icon":icon_schema(),"instrument":{"type":["object","null"]},"attention":attention_schema()}})
}
pub(super) fn thread_update_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{"thread":thread_ref_schema(),"title":{"type":"string","minLength":1},"description":{"type":"string"},"icon":icon_schema(),"showcased_artifact_id":{"type":["integer","null"],"minimum":1,"description":"Show one accessible artifact beside this Thread chat. Omit to preserve; null removes. Self or descendants only. This explicit reference grants the target scope read access while showcased; it creates no conversation card."},"instrument":{"type":["object","null"]},"attention":attention_schema()}})
}
pub(super) fn thread_list_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"properties":{"under":thread_ref_schema(),"depth":{"type":"integer","minimum":1,"maximum":8,"default":1},"limit":{"type":"integer","minimum":1,"maximum":100,"default":50},"after_id":{"type":"integer","minimum":0}}})
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

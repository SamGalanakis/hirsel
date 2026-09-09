use super::*;

fn attention_schema() -> Value {
    json!({"type":"string","enum":["quiet","needs_owner"]})
}

pub(super) fn thread_create_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["client_id","title"],"properties":{
        "client_id":{"type":"string","minLength":1,"description":"Stable creation key; reuse on retry."},
        "title":{"type":"string","minLength":1}, "description":{"type":"string"},
        "instrument":{"type":["object","null"],"description":"Optional constrained UI tree."},
        "attention":attention_schema()
    }})
}

pub(super) fn thread_update_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["thread_id"],"properties":{
        "thread_id":{"type":"integer","minimum":0},"title":{"type":"string","minLength":1},
        "description":{"type":"string"},"instrument":{"type":["object","null"]},
        "attention":attention_schema()
    }})
}

pub(super) fn thread_read_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["thread_id"],"properties":{
        "thread_id":{"type":"integer","minimum":0},"before_id":{"type":"integer","minimum":1},
        "limit":{"type":"integer","minimum":1,"maximum":100,"default":30}
    }})
}

pub(super) fn thread_activity_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["thread_id","kind","data"],"properties":{
        "thread_id":{"type":"integer","minimum":0},"kind":{"type":"string","minLength":1},
        "data":{"type":"object"}
    }})
}

pub(super) fn thread_result_schema() -> Value {
    json!({"type":"object","additionalProperties":false,"required":["thread_id","thread"],"properties":{
        "thread_id":{"type":"integer","minimum":0},"thread":{"type":"object"}
    }})
}

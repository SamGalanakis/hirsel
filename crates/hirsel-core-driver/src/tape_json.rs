//! JSON records for the physical tape inspector. Hex strings preserve binary
//! fields and 64-bit digests in clients that parse JSON numbers as doubles.
use super::Record;
use hirsel_core_ids::hex::encode;
use hirsel_tape_format::{Args, Frame, Out, TakePoint};
use serde_json::{Value, json};

fn args(value: &Args) -> Value {
    match value {
        Args::Literal(bytes) => json!({"literal_hex": encode(bytes)}),
        Args::Digest(hash) => json!({"digest_hex": format!("{hash:016x}")}),
        Args::Buffered { arguments, spare } => {
            json!({"arguments": args(arguments), "spare": spare})
        }
    }
}

fn outcome(out: &Out) -> Value {
    json!({"status": out.status, "value": out.value, "error": out.error,
           "written": out.written, "payload_hex": encode(&out.payload)})
}

fn frame(value: Frame) -> Value {
    match value {
        Frame::Wait { gap, task } => json!({"type":"WAIT","gap":gap,"task":task}),
        Frame::Wake { gap, task } => json!({"type":"WAKE","gap":gap,"task":task}),
        Frame::Cancel { gap, task } => json!({"type":"CANCEL","gap":gap,"task":task}),
        Frame::Retired {
            gap,
            task,
            stream,
            pool,
        } => {
            json!({"type":"RETIRED","gap":gap,"task":task,"stream":stream,"ordinal":pool >> 1,"intake":pool & 1 != 0})
        }
        Frame::Choose { gap, index } => json!({"type":"CHOOSE","gap":gap,"index":index}),
        Frame::Grant { gap, kind, args } => {
            json!({"type":"GRANT","gap":gap,"kind":kind,"args_hex":args.as_deref().map(encode)})
        }
        Frame::Issue {
            task,
            op,
            object,
            args: a,
        } => json!({"type":"ISSUE","task":task,"op":op,"object":object,"args":args(&a)}),
        Frame::Op {
            task,
            op,
            object,
            args: a,
            out,
        } => {
            json!({"type":"OP","task":task,"op":op,"object":object,"args":args(&a),"out":outcome(&out)})
        }
        Frame::Done { gap, task, out } => {
            json!({"type":"DONE","gap":gap,"task":task,"out":outcome(&out)})
        }
        Frame::Repeat { count } => json!({"type":"REPEAT","count":count}),
        Frame::Take {
            gap,
            point,
            count,
            body,
        } => {
            let point = match point {
                TakePoint::Operation => "operation",
                TakePoint::Pump => "pump",
            };
            json!({"type":"TAKE","gap":gap,"point":point,"count":count,"body_hex":encode(body.bytes())})
        }
        Frame::End { gap, ending } => {
            json!({"type":"END","gap":gap,"kind":ending.kind,"outcome":args(&ending.outcome)})
        }
        Frame::HelperEnd { gap, ending } => {
            json!({"type":"END","gap":gap,"kind":ending.kind(),"body_hex":encode(ending.body())})
        }
    }
}

pub(super) fn record(value: Record) -> Value {
    match value {
        Record::Header(h) => json!({
            "type":"HEADER", "schema":1, "format":hirsel_tape_format::FORMAT, "kind":h.kind,
            "identity_hex":encode(&h.identity), "revision":encode(&h.revision), "tape_id":encode(&h.tape_id),
            "limit":h.limit, "charge_policy":h.charge_policy, "bound":h.bound, "chooser":h.chooser,
            "arg_hex":encode(&h.arg), "net_rules":h.net_rules,
            "secret_grants":h.secret_grants, "cred_grants":h.cred_grants,
            "env":h.env.as_ref().map(|env| json!({"patterns":env.patterns,"entries":env.entries}))
        }),
        Record::Stream(id, path) => json!({"type":"STREAM","id":id,"path":path}),
        Record::Entry(entry) => {
            let body = entry.body();
            json!({"type":"ENTRY","parent":entry.parent,"spawn":entry.spawn,"descriptor_hex":encode(body.descriptor),
                    "n":body.n,"bound":body.bound,"value_hex":encode(body.value.bytes())})
        }
        Record::Frame(position, f) => {
            let mut f = frame(f);
            f["position"] = position.into();
            f
        }
        Record::Truncated => json!({"type":"TRUNCATED"}),
    }
}

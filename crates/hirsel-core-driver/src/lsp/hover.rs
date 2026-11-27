use super::documents::{Encoding, Text};
use super::navigation::range;
use super::snapshot::Fact;
use crate::reachability::authority::{Cards, Verdict, names};
use hirsel_core_ids::PackageId;
use serde_json::{Value, json};
use std::fmt::Write;

pub(super) struct Authority {
    pub(super) package: PackageId,
    pub(super) export: Option<String>,
    pub(super) ceiling: Option<u16>,
    pub(super) unresolved: bool,
}

pub(super) fn hover(fact: &Fact, text: &Text, cards: &Cards, encoding: Encoding) -> Option<Value> {
    let mut value = String::new();
    if let Some(declaration) = &fact.target {
        if let Some(signature) = &declaration.signature {
            write!(value, "```hirsel\n{signature}\n```").unwrap();
        }
        if let Some(doc) = &declaration.doc {
            write!(value, "\n\n{doc}").unwrap();
        }
    }
    if let Some(ty) = &fact.ty {
        write!(value, "\n\n`{ty}`").unwrap();
    }
    if let Some(authority) = &fact.authority {
        card(&mut value, cards, authority);
    } else if fact.unresolved {
        value.push_str("\n\n**Authority (§11)** · unknown");
    }
    let value = value.trim();
    (!value.is_empty()).then(|| {
        json!({
            "contents": {"kind": "markdown", "value": value},
            "range": range(text, fact.span, encoding),
        })
    })
}

fn card(out: &mut String, cards: &Cards, binding: &Authority) {
    let card = cards
        .get(&binding.package)
        .map(crate::package_audit::project_card);
    write!(
        out,
        "\n\n**Authority (§11)** · {}@{}",
        binding.package.name, binding.package.version,
    )
    .unwrap();
    if let Some(export) = &binding.export {
        write!(out, " · `{export}`").unwrap();
    }
    let carried = card
        .as_ref()
        .and_then(|card| card.carried.get(binding.export.as_deref()?));
    let mut reached = card
        .as_ref()
        .and_then(|card| card.reached.get(binding.export.as_deref()?));
    if binding.unresolved {
        reached = None;
    }
    if let Some(carried) = carried {
        write!(
            out,
            "\n\nCarried · takes [{}], returns [{}]",
            carried.takes.join(", "),
            carried.returns.join(", "),
        )
        .unwrap();
    } else if reached.is_some() {
        out.push_str("\n\nCarried · takes [], returns []");
    }
    if let Some(reached) = reached.filter(|reached| reached.unresolved.is_none()) {
        write!(
            out,
            "\n\nReached · [{}] · provides [{}]\n\nContained · [{}]",
            reached.kinds.join(", "),
            reached.provided.join(", "),
            reached
                .contained
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", "),
        )
        .unwrap();
    } else if binding.export.is_some() || binding.unresolved {
        out.push_str("\n\nReached · unknown");
    }
    let verdict = if binding.unresolved {
        String::from("unknown")
    } else {
        match card.map(|card| card.verdict) {
            Some(Verdict::Status(status)) => status.to_owned(),
            Some(Verdict::Kinds(kinds)) => kinds.join(", "),
            None => String::from("unknown"),
        }
    };
    write!(out, "\n\nPackage verdict · {verdict}").unwrap();
    if let Some(ceiling) = binding.ceiling {
        write!(
            out,
            "\n\nConsumer ceiling · [{}]",
            names(u32::from(ceiling)).join(", ")
        )
        .unwrap();
    }
}

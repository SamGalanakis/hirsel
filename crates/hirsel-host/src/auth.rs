use axum::http::{HeaderMap, header::AUTHORIZATION};
use subtle::ConstantTimeEq;

pub fn owner_token_matches(expected: &str, presented: &str) -> bool {
    !expected.is_empty()
        && !presented.is_empty()
        && bool::from(expected.as_bytes().ct_eq(presented.as_bytes()))
}

pub fn owner_bearer_matches(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split_once(' '))
        .is_some_and(|(scheme, token)| {
            scheme.eq_ignore_ascii_case("bearer") && owner_token_matches(expected, token)
        })
}

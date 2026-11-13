// Reviewed-by: langs-numeric-920 2026-11-13; same13 hand byte matchers, non-overlap scanning, owned tally keys/u64 counts and generated input. Overflow checks required.
// Literal alternatives use borrowed Cow carriers with owned-form cleanup, matching zero-capacity Hirsel String literals; delta approved by langs-numeric-920 2026-11-13; borrowed literals retain the owned-form carrier and cleanup.
// Matched twin of log_parser.hirsel. Reviewed below.
// kostya/LangArena3c9f84d benchmarks/log_parser.rs:12-149.
// Same 13 byte matchers and leftmost non-overlapping scans, owned tally keys.
mod text_support;
use std::borrow::Cow;
use std::collections::HashMap;
fn lower(b: u8) -> u8 {
    if b.is_ascii_uppercase() { b + 32 } else { b }
}
fn prefix(b: &[u8], i: usize, s: &str, fold: bool) -> bool {
    let p = s.as_bytes();
    p.len() <= b.len() - i
        && p.iter()
            .enumerate()
            .all(|(k, &c)| (if fold { lower(b[i + k]) } else { b[i + k] }) == c)
}
fn digit(b: u8) -> bool {
    b.is_ascii_digit()
}
fn alpha(b: u8) -> bool {
    let c = lower(b);
    c.is_ascii_lowercase()
}
fn word(b: u8) -> bool {
    alpha(b) || digit(b) || b == b'_'
}
fn digits(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && digit(b[i]) {
        i += 1;
    }
    i
}
fn blank(b: u8) -> bool {
    b == b' ' || (9..=13).contains(&b)
}
fn value_end(b: &[u8], mut i: usize) -> usize {
    while i < b.len() && b[i] != b'&' && b[i] != b'"' && !blank(b[i]) {
        i += 1;
    }
    i
}
fn email_local(b: u8) -> bool {
    alpha(b) || digit(b) || b"._%+-".contains(&b)
}
fn email_domain(b: u8) -> bool {
    alpha(b) || digit(b) || b == b'.' || b == b'-'
}
fn matched(b: &[u8], i: usize, pattern: usize) -> Option<usize> {
    let n = b.len();
    if pattern == 0 {
        if i + 5 <= n
            && b[i] == b' '
            && (b[i + 1] == b'4' || b[i + 1] == b'5')
            && digit(b[i + 2])
            && digit(b[i + 3])
            && b[i + 4] == b' '
        {
            return Some(i + 5);
        }
    } else if pattern == 1 {
        for s in [
            Cow::Borrowed("bot"), Cow::Borrowed("crawler"), Cow::Borrowed("scanner"), Cow::Borrowed("spider"), Cow::Borrowed("indexing"), Cow::Borrowed("crawl"), Cow::Borrowed("robot"),
        ] {
            if prefix(b, i, &s, true) {
                return Some(i + s.len());
            }
        }
    } else if pattern == 2 {
        for s in [Cow::Borrowed("etc/passwd"), Cow::Borrowed("wp-admin"), Cow::Borrowed("../")] {
            if prefix(b, i, &s, true) {
                return Some(i + s.len());
            }
        }
    } else if pattern == 3 {
        let mut pos = i;
        for _ in 0..3 {
            let end = digits(b, pos);
            if end == pos || end >= n || b[end] != b'.' {
                return None;
            }
            pos = end + 1;
        }
        if prefix(b, pos, "35", false) {
            return Some(pos + 2);
        }
    } else if pattern == 4 {
        if prefix(b, i, "/api/", false) {
            let mut j = i + 5;
            while j < n && b[j] != b' ' && b[j] != b'"' {
                j += 1;
            }
            if j > i + 5 {
                return Some(j);
            }
        }
    } else if pattern == 5 {
        if prefix(b, i, "POST ", false) {
            let mut j = i + 5;
            while j < n && b[j] != b' ' {
                j += 1;
            }
            if prefix(b, j, " HTTP", false) {
                return Some(j + 5);
            }
        }
    } else if pattern == 6 {
        if prefix(b, i, "/login", true) {
            return Some(i + 6);
        }
        if prefix(b, i, "/signin", true) {
            return Some(i + 7);
        }
    } else if pattern == 7 {
        for s in [Cow::Borrowed("get"), Cow::Borrowed("post"), Cow::Borrowed("put")] {
            if prefix(b, i, &s, true) {
                return Some(i + s.len());
            }
        }
    } else if pattern == 8 {
        let mut j = i;
        while j < n && email_local(b[j]) {
            j += 1;
        }
        if j == i || j >= n || b[j] != b'@' {
            return None;
        }
        let domain = j + 1;
        j = domain;
        while j < n && email_domain(b[j]) {
            j += 1;
        }
        let mut dot = j;
        while dot > domain {
            dot -= 1;
            if b[dot] == b'.' && dot > domain {
                let mut end = dot + 1;
                while end < n && alpha(b[end]) {
                    end += 1;
                }
                if end >= dot + 3 {
                    return Some(end);
                }
            }
        }
    } else if pattern == 9 {
        if prefix(b, i, "password=", false) {
            let end = value_end(b, i + 9);
            if end > i + 9 {
                return Some(end);
            }
        }
    } else if pattern == 10 {
        for s in [Cow::Borrowed("token="), Cow::Borrowed("apikey="), Cow::Borrowed("api_key="), Cow::Borrowed("api-key=")] {
            if prefix(b, i, &s, false) {
                let start = i + s.len();
                let end = value_end(b, start);
                if end > start {
                    return Some(end);
                }
            }
        }
    } else if pattern == 11 {
        for s in [Cow::Borrowed("sessionid="), Cow::Borrowed("session_id="), Cow::Borrowed("session-id=")] {
            if prefix(b, i, &s, false) {
                let start = i + s.len();
                let end = value_end(b, start);
                if end > start {
                    return Some(end);
                }
            }
        }
    } else if pattern == 12 {
        if b[i] != b'[' {
            return None;
        }
        let mut j = digits(b, i + 1);
        if j == i + 1 || j >= n || b[j] != b'/' {
            return None;
        }
        let start = j + 1;
        j = start;
        while j < n && word(b[j]) {
            j += 1;
        }
        if j == start || j >= n || b[j] != b'/' {
            return None;
        }
        let year = j + 1;
        j = digits(b, year);
        if j == year
            || j + 4 > n
            || b[j] != b':'
            || b[j + 1] != b'1'
            || b[j + 2] < b'3'
            || b[j + 2] > b'7'
            || b[j + 3] != b':'
        {
            return None;
        }
        let minute = j + 4;
        j = digits(b, minute);
        if j == minute || j >= n || b[j] != b':' {
            return None;
        }
        let second = j + 1;
        j = digits(b, second);
        if j == second || j + 2 > n || b[j] != b' ' || (b[j + 1] != b'+' && b[j + 1] != b'-') {
            return None;
        }
        let zone = j + 2;
        j = digits(b, zone);
        if j > zone && j < n && b[j] == b']' {
            return Some(j + 1);
        }
    }
    None
}
fn scan(log: &str) -> u32 {
    let names = [
        "errors",
        "bots",
        "suspicious",
        "ips",
        "api_calls",
        "post_requests",
        "auth_attempts",
        "methods",
        "emails",
        "passwords",
        "tokens",
        "sessions",
        "peak_hours",
    ];
    let b = log.as_bytes();
    let mut counts = HashMap::new();
    for (pattern, name) in names.iter().enumerate() {
        counts.insert((*name).to_owned(), 0u64);
        let mut i = 0;
        while i < b.len() {
            if let Some(end) = matched(b, i, pattern) {
                let count = counts.get_mut(*name).unwrap();
                *count = count.checked_add(1).unwrap();
                i = end;
            } else {
                i += 1;
            }
        }
    }
    let mut total = 0u64;
    for count in counts.values() {
        total = total.checked_add(*count).unwrap();
    }
    total as u32
}
fn generate(count: usize) -> String {
    let methods = ["GET", "POST", "PUT", "DELETE"];
    let paths = [
        "/index.html",
        "/api/users",
        "/admin",
        "/images/logo.png",
        "/etc/passwd",
        "/wp-admin/setup.php",
    ];
    let statuses = [200, 201, 301, 302, 400, 401, 403, 404, 500, 502, 503];
    let agents = ["Mozilla/5.0", "Googlebot/2.1", "curl/7.68.0", "scanner/2.0"];
    let users = [
        "john", "jane", "alex", "sarah", "mike", "anna", "david", "elena",
    ];
    let domains = [
        "example.com",
        "gmail.com",
        "yahoo.com",
        "hotmail.com",
        "company.org",
        "mail.ru",
    ];
    let mut log = String::with_capacity(count.checked_mul(200).unwrap());
    for i in 0..count {
        log.push_str(&format!(
            "192.168.1.{} - - [{}/Oct/2023:{}:55:36 +0000] \"{} ",
            i % 255 + 1,
            i % 31,
            i % 60,
            methods[i % 4]
        ));
        if i % 3 == 0 {
            log.push_str(&format!(
                "/login?email={}{}@{}&password=secret{}",
                users[i % 8],
                i % 100,
                domains[i % 6],
                i % 10000
            ));
        } else if i % 5 == 0 {
            log.push_str("/api/data?token=");
            for _ in 0..i % 3 + 1 {
                log.push_str("abcdef123456");
            }
        } else if i % 7 == 0 {
            log.push_str(&format!(
                "/user/profile?session_id=sess_{:x}",
                i.checked_mul(12345).unwrap()
            ));
        } else {
            log.push_str(paths[i % 6]);
        }
        log.push_str(&format!(
            " HTTP/1.1\" {} 2326 \"http://{}\" \"{}\"\n",
            statuses[i % 11],
            domains[i % 6],
            agents[i % 4]
        ));
    }
    log
}
fn main() {
    let a = text_support::arguments(&["lines_count"], false);
    let count: usize = a[1].parse().unwrap();
    let warmup: u64 = a[2].parse().unwrap();
    let iterations: u64 = a[3].parse().unwrap();
    let log = generate(count);
    let mut checksum = 0u32;
    for _ in 0..warmup.checked_add(iterations).unwrap() {
        checksum = checksum.wrapping_add(scan(&log));
    }
    text_support::output(checksum);
}

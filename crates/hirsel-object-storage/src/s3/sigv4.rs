//! AWS Signature Version 4, a structural port of botocore `auth.py` SigV4Auth
//! (botocore 4d9f375e71c1a97693d961cdefb1827bdaaccfb9: canonical headers :304,
//! canonical request :373, string to sign :408, key derivation :420) and of
//!
//! Deviations, each for the no-allocation rule: the canonical request is
//! written into a buffer the signer owns and reuses; the string to sign is
//! never materialised but fed straight into the HMAC; the signing key is
//! derived once per day (region and service are fixed per signer); headers
//! are ordered by an index sort over at most [`MAX_HEADERS`] entries rather
//! than collected into a map. The caller supplies the encoded path and the
//! already canonical (sorted, encoded) query, as the Hirsel signer's callers
//! do.

use sha2::{Digest, Sha256};

/// The most headers one request signs.
pub(crate) const MAX_HEADERS: usize = 16;

/// The SHA-256 of the empty payload, in lowercase hexadecimal.
pub(crate) const EMPTY_HASH: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

const HEX: &[u8; 16] = b"0123456789abcdef";

/// `bytes` as lowercase hexadecimal into `out`.
pub(crate) fn hex_into(bytes: &[u8; 32], out: &mut [u8; 64]) {
    for (i, b) in bytes.iter().enumerate() {
        out[2 * i] = HEX[(b >> 4) as usize];
        out[2 * i + 1] = HEX[(b & 15) as usize];
    }
}

/// HMAC-SHA256 (RFC 2104) of the concatenation of `parts` under `key`.
pub(crate) fn hmac(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > 64 {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    let mut pad = [0u8; 64];
    for (p, k) in pad.iter_mut().zip(block) {
        *p = k ^ 0x36;
    }
    inner.update(pad);
    for part in parts {
        inner.update(part);
    }
    let inner = inner.finalize();
    for (p, k) in pad.iter_mut().zip(block) {
        *p = k ^ 0x5c;
    }
    let mut outer = Sha256::new();
    outer.update(pad);
    outer.update(inner);
    outer.finalize().into()
}

/// One credential's signer for one region and service.
pub(crate) struct Signer {
    access_key: String,
    secret_key: Vec<u8>,
    region: String,
    service: String,
    /// The day (`YYYYMMDD`) `key` was derived for; zeros before the first.
    day: [u8; 8],
    key: [u8; 32],
    /// The last canonical request, kept for inspection and reuse.
    pub(crate) canonical: Vec<u8>,
}

impl Signer {
    pub(crate) fn new(access_key: &str, secret_key: &[u8], region: &str, service: &str) -> Self {
        Signer {
            access_key: access_key.into(),
            secret_key: secret_key.into(),
            region: region.into(),
            service: service.into(),
            day: [0; 8],
            key: [0; 32],
            canonical: Vec::with_capacity(1024),
        }
    }

    /// The signing key of `day`, derived when the day changes.
    fn key(&mut self, day: &[u8; 8]) -> [u8; 32] {
        if &self.day != day {
            let mut initial = [0u8; 4 + 128];
            let secret = &self.secret_key[..self.secret_key.len().min(128)];
            initial[..4].copy_from_slice(b"AWS4");
            initial[4..4 + secret.len()].copy_from_slice(secret);
            let day_key = hmac(&initial[..4 + secret.len()], &[day]);
            let region_key = hmac(&day_key, &[self.region.as_bytes()]);
            let service_key = hmac(&region_key, &[self.service.as_bytes()]);
            self.key = hmac(&service_key, &[b"aws4_request"]);
            self.day = *day;
        }
        self.key
    }

    /// Sign one request and append its `Authorization` value to `out`.
    /// `headers` are (name, value) in any order and case, `Host` and
    /// `X-Amz-Date` among them; each is signed. `stamp` is the
    /// `YYYYMMDDTHHMMSSZ` the `X-Amz-Date` header carries.
    pub(crate) fn sign(
        &mut self,
        method: &str,
        path: &[u8],
        query: &[u8],
        headers: &[(&str, &str)],
        payload_hash: &str,
        stamp: &[u8; 16],
        out: &mut Vec<u8>,
    ) {
        assert!(headers.len() <= MAX_HEADERS, "too many signed headers");
        // A stable insertion sort of header indices by lowercase name keeps
        // repeated headers in their given order, as botocore's do.
        let mut order = [0usize; MAX_HEADERS];
        let n = headers.len();
        for (i, o) in order.iter_mut().enumerate().take(n) {
            *o = i;
        }
        fn lower(s: &str) -> impl Iterator<Item = u8> + '_ {
            s.bytes().map(|b| b.to_ascii_lowercase())
        }
        for i in 1..n {
            let mut j = i;
            while j > 0 && lower(headers[order[j - 1]].0).gt(lower(headers[order[j]].0)) {
                order.swap(j - 1, j);
                j -= 1;
            }
        }
        let order = &order[..n];
        let same = |a: usize, b: usize| headers[a].0.eq_ignore_ascii_case(headers[b].0);
        let c = &mut self.canonical;
        c.clear();
        c.extend_from_slice(method.as_bytes());
        c.push(b'\n');
        c.extend_from_slice(path);
        c.push(b'\n');
        c.extend_from_slice(query);
        c.push(b'\n');
        for (k, &i) in order.iter().enumerate() {
            if k > 0 && same(order[k - 1], i) {
                c.push(b',');
            } else {
                c.extend(lower(headers[i].0));
                c.push(b':');
            }
            // botocore's `' '.join(value.split())`.
            for (w, word) in headers[i].1.split_ascii_whitespace().enumerate() {
                if w > 0 {
                    c.push(b' ');
                }
                c.extend_from_slice(word.as_bytes());
            }
            if k + 1 == n || !same(i, order[k + 1]) {
                c.push(b'\n');
            }
        }
        c.push(b'\n');
        let signed_from = c.len();
        for (k, &i) in order.iter().enumerate() {
            if k > 0 && same(order[k - 1], i) {
                continue;
            }
            if k > 0 {
                c.push(b';');
            }
            c.extend(lower(headers[i].0));
        }
        let signed_to = c.len();
        c.push(b'\n');
        c.extend_from_slice(payload_hash.as_bytes());

        let mut hashed = [0u8; 64];
        hex_into(&Sha256::digest(&self.canonical).into(), &mut hashed);
        let day: [u8; 8] = stamp[..8].try_into().unwrap();
        let key = self.key(&day);
        let scope_tail = [
            b"/".as_slice(),
            self.region.as_bytes(),
            b"/",
            self.service.as_bytes(),
            b"/aws4_request",
        ];
        let mut signature = [0u8; 64];
        hex_into(
            &hmac(
                &key,
                &[
                    b"AWS4-HMAC-SHA256\n",
                    stamp,
                    b"\n",
                    &day,
                    scope_tail[0],
                    scope_tail[1],
                    scope_tail[2],
                    scope_tail[3],
                    scope_tail[4],
                    b"\n",
                    &hashed,
                ],
            ),
            &mut signature,
        );
        out.extend_from_slice(b"AWS4-HMAC-SHA256 Credential=");
        out.extend_from_slice(self.access_key.as_bytes());
        out.push(b'/');
        out.extend_from_slice(&day);
        for part in scope_tail {
            out.extend_from_slice(part);
        }
        out.extend_from_slice(b", SignedHeaders=");
        out.extend_from_slice(&self.canonical[signed_from..signed_to]);
        out.extend_from_slice(b", Signature=");
        out.extend_from_slice(&signature);
    }
}

/// `YYYYMMDDTHHMMSSZ` of `seconds` since the Unix epoch (UTC), by Howard
/// Hinnant's `civil_from_days`.
pub(crate) fn amz_stamp(seconds: u64) -> [u8; 16] {
    let days = (seconds / 86_400) as i64;
    let secs = seconds % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let mut out = *b"00000000T000000Z";
    let mut put = |at: usize, width: usize, mut v: u64| {
        for i in (0..width).rev() {
            out[at + i] = b'0' + (v % 10) as u8;
            v /= 10;
        }
    };
    put(0, 4, year as u64);
    put(4, 2, month as u64);
    put(6, 2, day as u64);
    put(9, 2, secs / 3600);
    put(11, 2, secs / 60 % 60);
    put(13, 2, secs % 60);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// AWS's aws4_testsuite as botocore vendors it, the vectors
    /// `packages/s3@1/tests/aws-0..5.hir` check: (headers, query, canonical
    /// request, signature).
    #[test]
    fn the_aws_signature_vectors_sign_as_published() {
        let host = ("Host", "example.amazonaws.com");
        let date = ("X-Amz-Date", "20150830T123600Z");
        let cases: [(&[(&str, &str)], &str, &str, &str); 6] = [
            (
                &[host, date],
                "",
                "GET\n/\n\nhost:example.amazonaws.com\nx-amz-date:20150830T123600Z\n\nhost;x-amz-date\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "5fa00fa31553b73ebf1942676e86291e8372ff2a2260956d9b8aae1d763fbf31",
            ),
            (
                &[
                    host,
                    ("My-Header1", " value1"),
                    ("My-Header2", " \"a b c\""),
                    date,
                ],
                "",
                "GET\n/\n\nhost:example.amazonaws.com\nmy-header1:value1\nmy-header2:\"a b c\"\nx-amz-date:20150830T123600Z\n\nhost;my-header1;my-header2;x-amz-date\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "acc3ed3afb60bb290fc8d2dd0098b9911fcaa05412b367055dee359757a9c736",
            ),
            (
                &[
                    host,
                    ("My-Header1", "value2"),
                    ("My-Header1", "value2"),
                    ("My-Header1", "value1"),
                    date,
                ],
                "",
                "GET\n/\n\nhost:example.amazonaws.com\nmy-header1:value2,value2,value1\nx-amz-date:20150830T123600Z\n\nhost;my-header1;x-amz-date\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "c9d5ea9f3f72853aea855b47ea873832890dbdd183b4468f858259531a5138ea",
            ),
            (
                &[
                    host,
                    ("My-Header1", "value4"),
                    ("My-Header1", "value1"),
                    ("My-Header1", "value3"),
                    ("My-Header1", "value2"),
                    date,
                ],
                "",
                "GET\n/\n\nhost:example.amazonaws.com\nmy-header1:value4,value1,value3,value2\nx-amz-date:20150830T123600Z\n\nhost;my-header1;x-amz-date\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "08c7e5a9acfcfeb3ab6b2185e75ce8b1deb5e634ec47601a50643f830c755c01",
            ),
            (
                &[host, date],
                "Param1=value1&Param2=value2",
                "GET\n/\nParam1=value1&Param2=value2\nhost:example.amazonaws.com\nx-amz-date:20150830T123600Z\n\nhost;x-amz-date\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "b97d918cfa904a5beff61c982a1b6f458b799221646efd99d3219ec94cdf2500",
            ),
            (
                &[host, date],
                "Param1=value1&Param1=value2",
                "GET\n/\nParam1=value1&Param1=value2\nhost:example.amazonaws.com\nx-amz-date:20150830T123600Z\n\nhost;x-amz-date\ne3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
                "5772eed61e12b33fae39ee5e7012498b51d56abc0abb7c60486157bd471c4694",
            ),
        ];
        let mut signer = Signer::new(
            "AKIDEXAMPLE",
            b"wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY",
            "us-east-1",
            "service",
        );
        for (headers, query, canonical, signature) in cases {
            let mut out = Vec::new();
            signer.sign(
                "GET",
                b"/",
                query.as_bytes(),
                headers,
                EMPTY_HASH,
                b"20150830T123600Z",
                &mut out,
            );
            assert_eq!(std::str::from_utf8(&signer.canonical).unwrap(), canonical);
            let names = canonical.lines().nth_back(1).unwrap();
            assert_eq!(
                String::from_utf8(out).unwrap(),
                format!(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/us-east-1/service/aws4_request, SignedHeaders={names}, Signature={signature}"
                )
            );
        }
    }

    #[test]
    fn stamps_are_utc_civil_time() {
        // 2015-08-30T12:36:00Z, the vectors' date, and a leap day.
        assert_eq!(&amz_stamp(1_440_938_160), b"20150830T123600Z");
        assert_eq!(&amz_stamp(951_782_400), b"20000229T000000Z");
        assert_eq!(&amz_stamp(0), b"19700101T000000Z");
    }
}

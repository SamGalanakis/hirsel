//! A fixed-depth scanner for the two facts a ListObjectsV2 page carries that
//! the store needs: each `ListBucketResult/Contents/Key` and
//! `ListBucketResult/IsTruncated`. It is not an XML parser: it walks tags,
//! keeps a path of at most [`DEPTH`] element names, skips declarations,
//! comments and processing instructions, decodes the five predefined entities
//! and numeric character references in the text it keeps, and refuses
//! anything else (CDATA, DOCTYPE, deeper nesting, unbalanced tags). The field
//! names follow the ListObjectsV2 response of the S3 API reference and Garage
//! `src/api/s3/list.rs`.

/// The deepest element path the scanner follows.
const DEPTH: usize = 8;

/// The scanned page: keys in document order and the truncation flag.
pub(crate) struct Page {
    pub keys: Vec<String>,
    pub truncated: bool,
}

fn decode(text: &[u8], out: &mut String) -> Result<(), &'static str> {
    let mut i = 0;
    while i < text.len() {
        if text[i] != b'&' {
            let next = text[i..]
                .iter()
                .position(|&b| b == b'&')
                .map_or(text.len(), |p| i + p);
            out.push_str(
                std::str::from_utf8(&text[i..next]).map_err(|_| "listing text is not UTF-8")?,
            );
            i = next;
            continue;
        }
        let end = text[i..]
            .iter()
            .take(12)
            .position(|&b| b == b';')
            .ok_or("unterminated entity in listing")?
            + i;
        let name = &text[i + 1..end];
        let c = match name {
            b"amp" => '&',
            b"lt" => '<',
            b"gt" => '>',
            b"quot" => '"',
            b"apos" => '\'',
            [b'#', b'x', hex @ ..] => {
                u32::from_str_radix(std::str::from_utf8(hex).unwrap_or("!"), 16)
                    .ok()
                    .and_then(char::from_u32)
                    .ok_or("bad character reference in listing")?
            }
            [b'#', dec @ ..] => std::str::from_utf8(dec)
                .ok()
                .and_then(|d| d.parse().ok())
                .and_then(char::from_u32)
                .ok_or("bad character reference in listing")?,
            _ => return Err("unknown entity in listing"),
        };
        out.push(c);
        i = end + 1;
    }
    Ok(())
}

/// Scan one ListObjectsV2 response body.
pub(crate) fn scan(body: &[u8]) -> Result<Page, &'static str> {
    let mut path: [&[u8]; DEPTH] = [b""; DEPTH];
    let mut depth = 0;
    let mut keys = Vec::new();
    let mut truncated = None;
    let mut seen_root = false;
    let mut i = 0;
    while i < body.len() {
        if body[i] != b'<' {
            let next = body[i..]
                .iter()
                .position(|&b| b == b'<')
                .map_or(body.len(), |p| i + p);
            let text = &body[i..next];
            let at = &path[..depth];
            if at == [b"ListBucketResult".as_slice(), b"Contents", b"Key"] {
                let mut key = String::new();
                decode(text, &mut key)?;
                keys.push(key);
            } else if at == [b"ListBucketResult".as_slice(), b"IsTruncated"] {
                truncated = Some(match text {
                    b"true" => true,
                    b"false" => false,
                    _ => return Err("malformed IsTruncated"),
                });
            } else if depth == 0 && !text.iter().all(u8::is_ascii_whitespace) {
                return Err("text outside the listing element");
            }
            i = next;
            continue;
        }
        let rest = &body[i..];
        if rest.starts_with(b"<?") {
            i += find(rest, b"?>").ok_or("unterminated declaration")? + 2;
            continue;
        }
        if rest.starts_with(b"<!--") {
            i += find(rest, b"-->").ok_or("unterminated comment")? + 3;
            continue;
        }
        if rest.starts_with(b"<!") {
            return Err("DOCTYPE or CDATA in listing");
        }
        let close = find(rest, b">").ok_or("unterminated tag")?;
        let tag = &rest[1..close];
        i += close + 1;
        if let Some(name) = tag.strip_prefix(b"/") {
            let name = name.trim_ascii_end();
            if depth == 0 || path[depth - 1] != name {
                return Err("unbalanced tag in listing");
            }
            depth -= 1;
            // A Key element that held no text is an empty key.
            continue;
        }
        let self_closing = tag.ends_with(b"/");
        let tag = if self_closing {
            &tag[..tag.len() - 1]
        } else {
            tag
        };
        let name = tag
            .split(|b| b.is_ascii_whitespace())
            .next()
            .unwrap_or_default();
        if name.is_empty() {
            return Err("empty tag name in listing");
        }
        if depth == 0 {
            if seen_root || name != b"ListBucketResult" {
                return Err("listing root is not ListBucketResult");
            }
            seen_root = true;
        }
        if self_closing {
            continue;
        }
        if depth == DEPTH {
            return Err("listing nests too deeply");
        }
        path[depth] = name;
        depth += 1;
    }
    if depth != 0 || !seen_root {
        return Err("incomplete listing");
    }
    Ok(Page {
        keys,
        truncated: truncated.ok_or("listing has no IsTruncated")?,
    })
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_listing_page_yields_its_keys_and_truncation() {
        // Shaped as AWS's ListObjectsV2 example response, with the escapes a
        // server writes.
        let body = br#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/">
  <Name>bucket</Name><Prefix>p/h/</Prefix><KeyCount>2</KeyCount><MaxKeys>1000</MaxKeys>
  <IsTruncated>true</IsTruncated>
  <!-- a comment -->
  <Contents><Key>p/h/1</Key><ETag>&quot;9b2cf535f27731c974343645a3985328&quot;</ETag><Size>3</Size>
    <Owner><ID>x</ID></Owner><StorageClass>STANDARD</StorageClass></Contents>
  <Contents><Key>p/h/a&amp;b&#x2F;c&#47;</Key><ETag/><Size>0</Size></Contents>
</ListBucketResult>"#;
        let page = scan(body).unwrap();
        assert_eq!(page.keys, ["p/h/1", "p/h/a&b/c/"]);
        assert!(page.truncated);
        for bad in [
            &b"<ListBucketResult><IsTruncated>maybe</IsTruncated></ListBucketResult>"[..],
            b"<ListBucketResult><Contents><Key>a</Key></Contents>",
            b"<ListBucketResult><Contents></Key></Contents></ListBucketResult>",
            b"<Error><Code>AccessDenied</Code></Error>",
            b"<ListBucketResult><IsTruncated>false</IsTruncated><![CDATA[x]]></ListBucketResult>",
            b"<ListBucketResult><Contents><Key>a&bogus;</Key></Contents><IsTruncated>false</IsTruncated></ListBucketResult>",
            b"<ListBucketResult></ListBucketResult>",
            b"<ListBucketResult><a><b><c><d><e><f><g><h>x</h></g></f></e></d></c></b></a></ListBucketResult>",
        ] {
            assert!(scan(bad).is_err(), "{}", String::from_utf8_lossy(bad));
        }
    }
}

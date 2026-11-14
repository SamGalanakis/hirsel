//! Safe byte-string and list operations mirroring core's source bodies and growth.
//! Source: core.string, core.list, std.string::find. No UTF-8 recan on typed views.
use std::borrow::Cow;
use std::ops::{Deref, DerefMut};
#[derive(Debug, PartialEq, Eq)]
pub struct List<T>(std::vec::Vec<T>);
impl<T> Default for List<T> {
    fn default() -> Self {
        Self::new()
    }
}
impl<T> List<T> {
    pub fn new() -> Self {
        Self(std::vec::Vec::new())
    }
    pub fn with_capacity(n: usize) -> Self {
        Self(std::vec::Vec::with_capacity(n))
    }
    pub fn capacity(&self) -> usize {
        self.0.capacity()
    }
    pub fn reserve(&mut self, additional: usize) {
        let needed = self.len() + additional;
        if needed <= self.capacity() {
            return;
        }
        let cap = needed.max(4).max(self.capacity() * 2);
        let mut next = std::vec::Vec::with_capacity(cap);
        next.extend(self.0.drain(..));
        self.0 = next;
    }
    pub fn push(&mut self, x: T) {
        self.reserve(1);
        self.0.push(x)
    }
    pub fn pop(&mut self) -> Option<T> {
        self.0.pop()
    }
    pub fn remove(&mut self, i: usize) -> T {
        self.0.remove(i)
    }
    pub fn retain(&mut self, f: impl FnMut(&T) -> bool) {
        self.0.retain(f)
    }
    pub fn truncate(&mut self, n: usize) {
        self.0.truncate(n)
    }
    pub fn clear(&mut self) {
        self.0.clear()
    }
    pub fn resize(&mut self, n: usize, x: T)
    where
        T: Clone,
    {
        if n > self.len() {
            self.reserve(n - self.len())
        }
        self.0.resize(n, x)
    }
    pub fn extend_from_slice(&mut self, x: &[T])
    where
        T: Clone,
    {
        self.reserve(x.len());
        self.0.extend_from_slice(x)
    }
    pub fn into_std(self) -> std::vec::Vec<T> {
        self.0
    }
}
impl<T> Deref for List<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        &self.0
    }
}
impl<T> DerefMut for List<T> {
    fn deref_mut(&mut self) -> &mut [T] {
        &mut self.0
    }
}
impl<T> IntoIterator for List<T> {
    type Item = T;
    type IntoIter = std::vec::IntoIter<T>;
    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}
impl<'a, T> IntoIterator for &'a List<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}
impl<T: Clone> Clone for List<T> {
    fn clone(&self) -> Self {
        let mut x = Self::with_capacity(self.len());
        x.extend_from_slice(self);
        x
    }
}
impl<T> AsRef<[T]> for List<T> {
    fn as_ref(&self) -> &[T] {
        self
    }
}
impl<T: PartialEq, const N: usize> PartialEq<&[T; N]> for List<T> {
    fn eq(&self, x: &&[T; N]) -> bool {
        self.0.as_slice() == *x
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct View<'a>(pub &'a [u8]);
impl<'a> View<'a> {
    pub fn as_bytes(self) -> &'a [u8] {
        self.0
    }
    pub fn len(self) -> usize {
        self.0.len()
    }
    pub fn is_empty(self) -> bool {
        self.0.is_empty()
    }
    pub fn bytes(self) -> std::iter::Copied<std::slice::Iter<'a, u8>> {
        self.0.iter().copied()
    }
    pub fn slice(self, lo: usize, hi: usize) -> Self {
        let v = &self.0[lo..hi];
        let n = self.len();
        assert!(
            !((lo < n && (self.0[lo] & 0xc0) == 0x80) || (hi < n && (self.0[hi] & 0xc0) == 0x80)),
            "inside a UTF-8 scalar"
        );
        Self(v)
    }
    pub fn find(self, pattern: &str) -> Option<usize> {
        find(self.0, 0, pattern.as_bytes())
    }
    pub fn starts_with(self, p: &str) -> bool {
        let p = p.as_bytes();
        p.len() <= self.len() && self.0[..p.len()] == *p
    }
    pub fn strip_prefix(self, p: &str) -> Option<Self> {
        let p = p.as_bytes();
        if p.len() <= self.len() && self.0[..p.len()] == *p {
            Some(self.slice(p.len(), self.len()))
        } else {
            None
        }
    }
    pub fn split_once(self, p: &str) -> Option<(Self, Self)> {
        let p = p.as_bytes();
        find(self.0, 0, p).map(|i| (self.slice(0, i), self.slice(i + p.len(), self.len())))
    }
    pub fn to_owned(self) -> Text {
        Text::copy(self)
    }
}
impl Deref for View<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.0
    }
}
impl AsRef<[u8]> for View<'_> {
    fn as_ref(&self) -> &[u8] {
        self.0
    }
}
impl<'a> From<&'a str> for View<'a> {
    fn from(x: &'a str) -> Self {
        Self(x.as_bytes())
    }
}
impl PartialEq<&str> for View<'_> {
    fn eq(&self, x: &&str) -> bool {
        self.0 == x.as_bytes()
    }
}
impl PartialEq<View<'_>> for &[u8] {
    fn eq(&self, x: &View<'_>) -> bool {
        *self == x.0
    }
}
impl PartialEq<View<'_>> for Text {
    fn eq(&self, x: &View<'_>) -> bool {
        self.as_bytes() == x.0
    }
}
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Text(Cow<'static, [u8]>);
impl Text {
    pub fn new() -> Self {
        Self(Cow::Owned(std::vec::Vec::new()))
    }
    pub fn literal(s: &'static str) -> Self {
        Self(Cow::Borrowed(s.as_bytes()))
    }
    pub fn with_capacity(n: usize) -> Self {
        Self(Cow::Owned(std::vec::Vec::with_capacity(n)))
    }
    pub fn from_bytes(v: List<u8>) -> Self {
        Self(Cow::Owned(v.into_std()))
    }
    pub fn copy(s: impl AsRef<[u8]>) -> Self {
        let s = s.as_ref();
        let mut out = Self::with_capacity(s.len());
        out.push_str(s);
        out
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
    pub fn view(&self) -> View<'_> {
        View(self.as_bytes())
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
    pub fn capacity(&self) -> usize {
        match &self.0 {
            Cow::Borrowed(_) => 0,
            Cow::Owned(v) => v.capacity(),
        }
    }
    pub fn reserve(&mut self, additional: usize) {
        let need = self.len() + additional;
        if need <= self.capacity() {
            return;
        }
        let cap = need.max(4).max(self.capacity() * 2);
        let mut v = std::vec::Vec::with_capacity(cap);
        v.extend_from_slice(&self.0);
        self.0 = Cow::Owned(v)
    }
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        match &mut self.0 {
            Cow::Owned(v) => v,
            Cow::Borrowed(_) => panic!("static bytes are immutable"),
        }
    }
    pub fn push_str(&mut self, s: impl AsRef<[u8]>) {
        let s = s.as_ref();
        self.reserve(s.len());
        match &mut self.0 {
            Cow::Owned(v) => v.extend_from_slice(s),
            _ => assert!(s.is_empty()),
        }
    }
    pub fn push(&mut self, c: char) {
        push_scalar(self, c as u32)
    }
    pub fn make_ascii_uppercase(&mut self) {
        self.bytes_mut().make_ascii_uppercase()
    }
    pub fn is_char_boundary(&self, i: usize) -> bool {
        i == self.len()
            || (i < self.len() && (self.as_bytes()[i] < 0x80 || self.as_bytes()[i] >= 0xc0))
    }
    pub fn into_bytes(self) -> List<u8> {
        match self.0 {
            Cow::Owned(v) => List(v),
            Cow::Borrowed(s) => {
                let mut v = List::with_capacity(s.len());
                v.extend_from_slice(s);
                v
            }
        }
    }
}
impl Clone for Text {
    fn clone(&self) -> Self {
        match &self.0 {
            Cow::Borrowed(s) => Self(Cow::Borrowed(s)),
            Cow::Owned(_) => Self::copy(self),
        }
    }
}
impl AsRef<[u8]> for Text {
    fn as_ref(&self) -> &[u8] {
        self.as_bytes()
    }
}
impl Deref for Text {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        self.as_bytes()
    }
}
impl PartialEq<&str> for Text {
    fn eq(&self, x: &&str) -> bool {
        self.as_bytes() == x.as_bytes()
    }
}
impl From<&'static str> for Text {
    fn from(x: &'static str) -> Self {
        Self::literal(x)
    }
}
impl<'a> From<&'a Text> for View<'a> {
    fn from(x: &'a Text) -> Self {
        x.view()
    }
}
impl<'a> From<&'a View<'a>> for View<'a> {
    fn from(x: &'a View<'a>) -> Self {
        *x
    }
}
pub fn word(s: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(s[i..i + 8].try_into().unwrap())
}
pub fn find(s: &[u8], start: usize, p: &[u8]) -> Option<usize> {
    let hay = &s[start..];
    let m = p.len();
    let rest = hay.len();
    if m > rest {
        return None;
    }
    if m == 0 {
        return Some(start);
    }
    let first = p[0];
    let spread = u64::from(first) * 0x0101010101010101;
    let last = rest - m;
    let mut i = 0;
    while i <= last {
        while i + 16 <= rest {
            let v0 = word(hay, i) ^ spread;
            let v1 = word(hay, i + 8) ^ spread;
            let z0 = v0.wrapping_sub(0x0101010101010101) & !v0 & 0x8080808080808080;
            let z1 = v1.wrapping_sub(0x0101010101010101) & !v1 & 0x8080808080808080;
            if z0 | z1 != 0 {
                let low = z0.trailing_zeros() as usize;
                let high = z1.trailing_zeros() as usize;
                i += if z0 != 0 { low >> 3 } else { 8 + (high >> 3) };
                break;
            }
            i += 16
        }
        while i <= last && hay[i] != first {
            i += 1
        }
        if i > last {
            return None;
        }
        let mut k = 1;
        while k < m && hay[i + k] == p[k] {
            k += 1
        }
        if k == m {
            return Some(start + i);
        }
        i += 1
    }
    None
}
pub fn utf8(s: &[u8]) -> bool {
    let mut i = 0;
    let n = s.len();
    while i < n {
        let x = s[i];
        if x < 128 {
            if i & 7 == 0 {
                while i + 16 <= n && (word(s, i) | word(s, i + 8)) & 0x8080808080808080 == 0 {
                    i += 16
                }
                while i < n && s[i] < 128 {
                    i += 1
                }
            } else {
                i += 1
            }
            continue;
        }
        if x < 0xe0 {
            if x < 0xc2 || n - i < 2 || s[i + 1] & 0xc0 != 0x80 {
                return false;
            }
            i += 2;
        } else if x < 0xf0 {
            if n - i < 3 {
                return false;
            }
            let y = s[i + 1];
            let mut lo = 0x80;
            if x == 0xe0 {
                lo = 0xa0
            }
            let mut hi = 0xbf;
            if x == 0xed {
                hi = 0x9f
            }
            if y < lo || y > hi || s[i + 2] & 0xc0 != 0x80 {
                return false;
            }
            i += 3;
        } else {
            if x > 0xf4 || n - i < 4 {
                return false;
            }
            let y = s[i + 1];
            let mut lo = 0x80;
            if x == 0xf0 {
                lo = 0x90
            }
            let mut hi = 0xbf;
            if x == 0xf4 {
                hi = 0x8f
            }
            if y < lo || y > hi || s[i + 2] & 0xc0 != 0x80 || s[i + 3] & 0xc0 != 0x80 {
                return false;
            }
            i += 4;
        }
    }
    true
}
pub fn push_scalar(s: &mut Text, v: u32) {
    if v < 128 && s.len() < s.capacity() {
        if let Cow::Owned(b) = &mut s.0 {
            b.push(v as u8);
            return;
        }
    }
    let w = if v < 128 {
        1
    } else if v < 2048 {
        2
    } else if v < 65536 {
        3
    } else {
        4
    };
    s.reserve(w);
    let v8 = |x: u32| x as u8;
    match &mut s.0 {
        Cow::Owned(b) => match w {
            1 => b.push(v8(v)),
            2 => {
                b.push(v8(0xc0 | (v >> 6)));
                b.push(v8(0x80 | (v & 63)))
            }
            3 => {
                b.push(v8(0xe0 | (v >> 12)));
                b.push(v8(0x80 | ((v >> 6) & 63)));
                b.push(v8(0x80 | (v & 63)))
            }
            _ => {
                b.push(v8(0xf0 | (v >> 18)));
                b.push(v8(0x80 | ((v >> 12) & 63)));
                b.push(v8(0x80 | ((v >> 6) & 63)));
                b.push(v8(0x80 | (v & 63)))
            }
        },
        _ => unreachable!(),
    }
}
pub fn decimal(s: &mut Text, n: u64) {
    let low = ((64 - u64::from(n.leading_zeros())) * 1233) >> 12;
    let reach = match low {
        0 => 0,
        1 => 10,
        2 => 100,
        3 => 1000,
        4 => 10000,
        5 => 100000,
        6 => 1000000,
        7 => 10000000,
        8 => 100000000,
        9 => 1000000000,
        10 => 10000000000,
        11 => 100000000000,
        12 => 1000000000000,
        13 => 10000000000000,
        14 => 100000000000000,
        15 => 1000000000000000,
        16 => 10000000000000000,
        17 => 100000000000000000,
        18 => 1000000000000000000,
        _ => 10000000000000000000,
    };
    let w = (if n >= reach { low + 1 } else { low }) as usize;
    s.push_str(match w {
        1 => &b"0"[..],
        2 => &b"00"[..],
        3 => &b"000"[..],
        4 => &b"0000"[..],
        5 => &b"00000"[..],
        6 => &b"000000"[..],
        7 => &b"0000000"[..],
        8 => &b"00000000"[..],
        9 => &b"000000000"[..],
        10 => &b"0000000000"[..],
        11 => &b"00000000000"[..],
        12 => &b"000000000000"[..],
        13 => &b"0000000000000"[..],
        14 => &b"00000000000000"[..],
        15 => &b"000000000000000"[..],
        16 => &b"0000000000000000"[..],
        17 => &b"00000000000000000"[..],
        18 => &b"000000000000000000"[..],
        19 => &b"0000000000000000000"[..],
        _ => &b"00000000000000000000"[..],
    });
    let b = s.bytes_mut();
    let base = b.len() - w;
    let mut i = b.len();
    let mut m = n;
    while m >= 100000000 {
        let v = m % 100000000;
        m /= 100000000;
        i -= 8;
        let x = v / 10000 | ((v % 10000) << 32);
        let y = (x.wrapping_mul(10486) >> 20) & 0x0000007f0000007f;
        let z = y | (x.wrapping_sub(y.wrapping_mul(100)) << 16);
        let t = (z.wrapping_mul(103) >> 10) & 0x000f000f000f000f;
        let d = (t | (z.wrapping_sub(t.wrapping_mul(10)) << 8)) | 0x3030303030303030;
        b[i + 7] = (d >> 56) as u8;
        b[i] = (d & 255) as u8;
        b[i + 1] = ((d >> 8) & 255) as u8;
        b[i + 2] = ((d >> 16) & 255) as u8;
        b[i + 3] = ((d >> 24) & 255) as u8;
        b[i + 4] = ((d >> 32) & 255) as u8;
        b[i + 5] = ((d >> 40) & 255) as u8;
        b[i + 6] = ((d >> 48) & 255) as u8
    }
    if m >= 10000 {
        let r = m % 10000;
        m /= 10000;
        i -= 4;
        let x = r / 100 | ((r % 100) << 16);
        let t = (x.wrapping_mul(103) >> 10) & 0x000f000f;
        let d = (t | (x.wrapping_sub(t.wrapping_mul(10)) << 8)) | 0x30303030;
        b[i + 3] = (d >> 24) as u8;
        b[i] = (d & 255) as u8;
        b[i + 1] = ((d >> 8) & 255) as u8;
        b[i + 2] = ((d >> 16) & 255) as u8
    }
    if m >= 1000 {
        let x = m / 100 | ((m % 100) << 16);
        let t = (x.wrapping_mul(103) >> 10) & 0x000f000f;
        let d = (t | (x.wrapping_sub(t.wrapping_mul(10)) << 8)) | 0x30303030;
        b[base + 3] = (d >> 24) as u8;
        b[base] = (d & 255) as u8;
        b[base + 1] = ((d >> 8) & 255) as u8;
        b[base + 2] = ((d >> 16) & 255) as u8
    } else {
        if m >= 100 {
            b[base] = b'0' + (m / 100) as u8;
            m %= 100;
            i = base + 1
        } else {
            i = base
        }
        if m >= 10 || i > base {
            b[i] = b'0' + (m / 10) as u8;
            b[i + 1] = b'0' + (m % 10) as u8
        } else {
            b[i] = b'0' + m as u8
        }
    }
}

impl PartialEq<str> for Text {
    fn eq(&self, s: &str) -> bool {
        self.as_bytes() == s.as_bytes()
    }
}
impl<T: PartialEq> PartialEq<&[T]> for List<T> {
    fn eq(&self, x: &&[T]) -> bool {
        self.0.as_slice() == *x
    }
}

pub fn encode_bytes(b: &mut List<u8>, v: u32) {
    if v < 128 && b.len() < b.capacity() {
        b.push(v as u8);
        return;
    }
    let w = if v < 128 {
        1
    } else if v < 2048 {
        2
    } else if v < 65536 {
        3
    } else {
        4
    };
    b.reserve(w);
    match w {
        1 => b.push(v as u8),
        2 => {
            b.push((0xc0 | (v >> 6)) as u8);
            b.push((0x80 | (v & 63)) as u8)
        }
        3 => {
            b.push((0xe0 | (v >> 12)) as u8);
            b.push((0x80 | ((v >> 6) & 63)) as u8);
            b.push((0x80 | (v & 63)) as u8)
        }
        _ => {
            b.push((0xf0 | (v >> 18)) as u8);
            b.push((0x80 | ((v >> 12) & 63)) as u8);
            b.push((0x80 | ((v >> 6) & 63)) as u8);
            b.push((0x80 | (v & 63)) as u8)
        }
    }
}
pub fn hex4(out: &mut List<u8>, n: u64) {
    let mut digits = [0u8; 64];
    let mut count = 0;
    let mut v = n;
    loop {
        let d = u8::try_from(v % 16).unwrap();
        digits[count] = if d < 10 { b'0' + d } else { b'a' + d - 10 };
        count += 1;
        v /= 16;
        if v == 0 {
            break;
        }
    }
    let mut padded = count;
    while padded < 4 {
        encode_bytes(out, u32::from(b'0'));
        padded += 1
    }
    while count > 0 {
        count -= 1;
        encode_bytes(out, u32::from(digits[count]))
    }
}

pub fn next_scalar(s: &[u8], at: usize) -> Option<(char, usize)> {
    let n = s.len();
    if at == n {
        return None;
    }
    let x = s[at];
    assert!(x & 0xc0 != 0x80, "inside a UTF-8 scalar");
    let w = if x < 0x80 {
        1
    } else if x < 0xe0 {
        2
    } else if x < 0xf0 {
        3
    } else {
        4
    };
    let lead = match w {
        1 => x,
        2 => x & 0x1f,
        3 => x & 0x0f,
        _ => x & 7,
    };
    let mut v = u32::from(lead);
    for k in 1..w {
        let y = u32::from(s[at + k] & 0x3f);
        v = (v << 6) | y
    }
    Some((char::from_u32(v).unwrap(), at + w))
}

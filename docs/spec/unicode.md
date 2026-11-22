# Unicode text

This chapter specifies std's Unicode character classification and case conversion: the
`std.char` members `UNICODE_VERSION`, `Category`, `category`, `is_alphabetic`, `is_letter`,
`is_numeric`, `is_decimal`, `is_alphanumeric`, `is_whitespace`, `is_uppercase`,
`is_lowercase`, and the `std.string` members `push_upper`, `push_lower`, `to_upper` and
`to_lower`. `char` and `string` extend core, so their members are written
bare everywhere (`char.is_letter(c)`, `string.to_lower(s)`), and the type is `char.Category`.

**Rule numbers.** UNI-n are std Unicode rules.

### UNI-1: One pinned Unicode version

Every answer in this chapter is a function of its argument and of the Unicode Character
Database, version 18.0.0, alone: the same on every engine, build, optimisation level and host,
and in replay ([SEM-8](semantics.md#sem-8-determinism)).

- **The data.** The tables are derived from these files of
  `https://www.unicode.org/Public/18.0.0/ucd/`, identified by their SHA-256:

  | file | SHA-256 | used for |
  |---|---|---|
  | `UnicodeData.txt` | `0736451de439ae7baf1425136617da495e09ee5afbe6e394374db7009ea08950` | General_Category (field 2), simple uppercase (field 12) and lowercase (field 13) mappings |
  | `SpecialCasing.txt` | `8538dea57c184f1ef3783885ea79677b10f6efa06423717157e63712f14d1ad2` | full case mappings, the entries with no condition only |
  | `DerivedCoreProperties.txt` | `09c928886a178fcafd93c29e4bd59073a058e5a100b716d425cb563ab50f68c9` | Alphabetic, Lowercase, Uppercase |
  | `PropList.txt` | `f438f532e8737bb8a2702126cdf9c4af5e357c58c7acf9d9eb2fc7c1a1d955d6` | White_Space |

  The files are © Unicode, Inc. and licensed under the Unicode License v3
  (`https://www.unicode.org/license.txt`, SPDX `Unicode-3.0`). std's source states the notice
  beside the tables, as it does beside the regex tables, so a distribution's notices can cite it.
- **One version for std.** std's regex tables are Unicode 18.0.0 too: the regex crate commit that
  [regex](regex.md) pins generates its tables from the 18.0.0 database. So `\w`, `\d`, `\s` and `(?i)` in
  std regex and the functions here read the same database, and the version moves for all of std
  at once or not at all. `char.UNICODE_VERSION` is `(18, 0, 0)`.
- **std code, std constants.** The tables are std constants
  ([LANG-20](language.md#lang-20-constants)) and the functions are ordinary std bodies over
  them. They add no core function and no engine operation, and no engine, evaluator or
  runtime answers them from a table of its own (the host's C library, ICU, or the
  Unicode version of the Rust toolchain that built the compiler). A toolchain whose std has
  other tables is a different std, and its programs are different programs.
- **No locale.** Nothing here reads a locale, an environment variable or any other input. The
  SpecialCasing entries with a condition (the `tr`, `az` and `lt` language tailorings, and the
  `Final_Sigma`, `After_Soft_Dotted`, `More_Above`, `Before_Dot` and `After_I` contexts) are
  not used. Adding any of them later changes answers, so it would be a change to this rule,
  not an addition beside it.

```hirsel
pub const UNICODE_VERSION: (u8, u8, u8) = (18, 0, 0)
```

### UNI-2: Classification

Each classifier takes a `char` and returns whether it has a property of the pinned database
(UNI-1), with no allocation and no failure.

```hirsel
pub fn is_alphabetic(c: char) -> bool
pub fn is_letter(c: char) -> bool
pub fn is_numeric(c: char) -> bool
pub fn is_decimal(c: char) -> bool
pub fn is_alphanumeric(c: char) -> bool
pub fn is_whitespace(c: char) -> bool
pub fn is_uppercase(c: char) -> bool
pub fn is_lowercase(c: char) -> bool
```

| function | true exactly when `c` | as in |
|---|---|---|
| `is_alphabetic` | has the derived property Alphabetic | Rust `char::is_alphabetic`, Swift `Character.isLetter` |
| `is_letter` | has General_Category `Lu`, `Ll`, `Lt`, `Lm` or `Lo` | Go `unicode.IsLetter` |
| `is_numeric` | has General_Category `Nd`, `Nl` or `No` | Rust `char::is_numeric`, Go `unicode.IsNumber` |
| `is_decimal` | has General_Category `Nd` | Go `unicode.IsDigit` |
| `is_alphanumeric` | `is_alphabetic(c) \|\| is_numeric(c)` | Rust `char::is_alphanumeric` |
| `is_whitespace` | has the property White_Space | Rust `char::is_whitespace` |
| `is_uppercase` | has the derived property Uppercase | Rust `char::is_uppercase` |
| `is_lowercase` | has the derived property Lowercase | Rust `char::is_lowercase` |

- **Alphabetic is wider than letter.** Alphabetic is the letters, the letter numbers (`Nl`) and
  Other_Alphabetic, which includes combining marks such as U+0345. So `is_alphabetic('Ⅷ')` is
  true and `is_letter('Ⅷ')` is false. "A run of Unicode letters or digits" is
  `is_letter(c) || is_decimal(c)`.
- **Case properties are not categories.** Uppercase is `Lu` plus Other_Uppercase and Lowercase
  is `Ll` plus Other_Lowercase, so a titlecase letter (`Lt`, such as `ǅ`) is neither, `ª` is
  lowercase, and `Ⅷ` is uppercase.
- **Unassigned scalars.** A scalar the database does not assign has category `Cn` and none of
  these properties.
- **Regex agrees.** For every scalar `c`, std regex's Unicode `\w` matches `c` exactly when
  `is_alphabetic(c)`, or `category(c)` is `Mn`, `Mc`, `Me`, `Nd` or `Pc`, or `c` is U+200C or
  U+200D (the UTS #18 word class of [regex](regex.md)). `\d` matches exactly when
  `is_decimal(c)`, and `\s` exactly when `is_whitespace(c)`.

```hirsel
fn word_char(c: char) -> bool {
    char.is_letter(c) || char.is_decimal(c)
}
```

### UNI-3: General category

`char.category(c)` returns the General_Category of `c` in the pinned database (UNI-1), as a
`char.Category` named by the database's short alias.

```hirsel
pub type Category: key = Lu | Ll | Lt | Lm | Lo | Mn | Mc | Me | Nd | Nl | No | Pc | Pd | Ps | Pe | Pi | Pf | Po | Sm | Sc | Sk | So | Zs | Zl | Zp | Cc | Cf | Co | Cn
pub fn category(c: char) -> Category
```

- The variants are the 30 General_Category values in the order UAX #44 lists them, less `Cs`:
  a `char` is a scalar value and never a surrogate, so `Cs` cannot be returned and is not
  declared.
- `Cn` is every scalar `UnicodeData.txt` does not list, noncharacters such as U+FFFF included.
  A scalar inside a `<..., First>` to `<..., Last>` range takes that range's category: `Lo` for
  U+4E00, `Co` for U+F0000.
- A program matches on the result with the expected type selecting the variants
  ([LANG-7](language.md#lang-7-names)), so `Lu` in a `match` on `char.category(c)` is never
  confused with a type of its own module.

```hirsel
fn is_symbol(c: char) -> bool {
    match char.category(c) {
        Sm => true,
        Sc => true,
        Sk => true,
        So => true,
        _ => false,
    }
}
```

### UNI-4: Case conversion

A scalar's uppercase and lowercase are full case mappings, possibly several scalars, and a
string's is the concatenation of its scalars' mappings, in order.

```hirsel
pub fn push_upper(mut out: String, c: char)
pub fn push_lower(mut out: String, c: char)
pub fn to_upper(s: String) -> String
pub fn to_lower(s: String) -> String
```

- **One scalar.** `upper(c)` is the `<upper>` field of the `SpecialCasing.txt` entry for `c` with
  no condition, when there is one. Otherwise it is the scalar in field 12 of `c`'s
  `UnicodeData.txt` line, when that field is not empty, and otherwise `c` itself. `lower(c)`
  is the same with the `<lower>` field and field 13. A SpecialCasing entry overrides
  UnicodeData even where it maps `c` to itself. `upper(c)` has one to three scalars and
  `lower(c)` one or two; U+0130 is the only scalar whose lowercase has two.
- **Appending.** `string.push_upper(mut out, c)` appends `upper(c)` to `out`, and
  `string.push_lower` appends `lower(c)`, as `string.push_char(mut out, c)` appends `c`. `out`
  gains all of the mapping or, when its growth is refused, none of it. A scalar's mapping is
  text, so it lives with the other appenders, and the case tables belong to `string` alone.
- **Strings.** `string.to_upper(s)` returns a new `String` holding `upper(c)` for each scalar `c` of `s`
  in order, and `to_lower(s)` the same with `lower(c)`. `s` is only read.
- **Concatenation law.** For all strings `a` and `b`, `to_lower(a + b)` is `to_lower(a) +
  to_lower(b)`, and the same for `to_upper`. No scalar's mapping depends on its neighbours, so
  `to_lower("ΣΑΣ")` is `"σασ"`: the final-sigma rule that would end it in `ς` is a context of
  UNI-1's excluded set. Rust's `str::to_lowercase` applies that rule; std does not.
- **Not round trips.** Neither conversion inverts the other: `to_lower(to_upper("ß"))` is
  `"ss"`, and `to_upper(to_lower("İ"))` is `"I\u{307}"`, not `"İ"`. Case conversion is not
  case folding, and std has no folding operation outside regex `(?i)`.
- **Titlecase** is not provided.

```hirsel
fn key(word: String) -> String {
    string.to_lower(word)   // "Straße" and "STRASSE" give "straße" and "strasse": different keys
}
```

### UNI-5: ASCII variants

The ASCII operations stay, and on ASCII input agree with the Unicode ones everywhere except
whitespace.

- `char.to_ascii_upper`, `char.to_ascii_lower`, `char.is_ascii_alpha`, `char.is_ascii_digit`,
  `char.is_ascii_whitespace`, `string.make_ascii_upper` and `string.make_ascii_lower` keep their
  contracts. They read no table and change only ASCII.
- For an ASCII `c`: `upper(c)` is `to_ascii_upper(c)` alone, `lower(c)` is `to_ascii_lower(c)`
  alone, `is_alphabetic(c)` and `is_letter(c)` are `is_ascii_alpha(c)`, and `is_decimal(c)` and
  `is_numeric(c)` are `is_ascii_digit(c)`. So for an ASCII `s`, `to_upper(s)` has the bytes that
  `make_ascii_upper` gives a copy of `s`.
- `is_whitespace(c)` is true for U+000B LINE TABULATION, which `is_ascii_whitespace` (the
  WhatWG set) leaves out. It is also true for U+0085 and U+00A0, which are not ASCII.
- A non-ASCII scalar can map to ASCII: `lower('K')` (U+212A KELVIN SIGN) is `k`.

### UNI-6: Charges, steps and failures

The operations are ordinary std code: their charges are the
[MEM-24](budgets.md#mem-24-logical-charges) charges of their source and their steps those of
[MEM-30](budgets.md#mem-30-step-bounds). This rule fixes what a caller can rely on.

- **Tables.** The tables are std constants, never charged, and reading them admits nothing.
- **Classifiers and `category`** admit nothing and cannot fail. Each takes a number of steps
  bounded by a constant that depends on the tables alone, not on `c`.
- **`string.push_upper(mut out, c)` and `string.push_lower(mut out, c)`** reserve the mapping's UTF-8 length
  in `out` once, as `string.reserve` does, and then admit nothing. A refused reservation raises
  `Capacity` with `out` unchanged. A static `out` is copied into new storage first, as by
  `string.push_char` ([core](core.md)). Each takes a constant number of steps.
- **`string.to_upper(s)` and `string.to_lower(s)`** admit the result's storage once, with capacity exactly
  its byte length (charged rounded up to 8), and nothing else; an empty result admits nothing. A refusal raises
  `Capacity` and admits nothing. The result has at most three bytes for each byte of `s` (U+0390,
  two bytes, uppercases to six), so its length never overflows. Each takes
  O(`string.byte_len(s)` + 1) steps.
- **Failures.** The operations fail only with `Capacity`, for the admissions above, and
  `OutOfTime`, at a step under an exhausted bound. They have no `Fail` and no `Bounds`.
- **Determinism.** The steps follow from std's source and the argument alone, so a bound stops
  these operations at the same step on every engine, build and optimisation level, and in replay.

### UNI-7: Worked examples

Each row is read from the UNI-1 files, not computed by std; the source column cites the line, abridged. An
implementation is correct on these rows and on the laws of UNI-2 to UNI-5.

| input | operation | result | source |
|---|---|---|---|
| U+00DF `ß` | `upper` | `SS` (U+0053 U+0053) | SpecialCasing `00DF; 00DF; 0053 0073; 0053 0053;` |
| U+00DF `ß` | `lower`, `is_lowercase`, `category` | `ß`, true, `Ll` | UnicodeData `00DF;...;Ll;...;;;;` |
| U+1E9E `ẞ` | `lower`, `upper` | `ß`, `ẞ` | UnicodeData `1E9E;...;Lu;...;;;00DF;` |
| U+0130 `İ` | `lower` | U+0069 U+0307 | SpecialCasing `0130; 0069 0307; 0130; 0130;` (the `tr` and `az` entries map it to U+0069 and are excluded) |
| U+0049 `I` | `lower` | U+0069 `i`, never U+0131 | UnicodeData `0049;...;0069;` (the `tr` entry is excluded) |
| U+0390 `ΐ` | `upper` | U+0399 U+0308 U+0301 | SpecialCasing `0390; 0390; 0399 0308 0301; 0399 0308 0301;` |
| U+FB03 `ﬃ` | `upper` | `FFI` | SpecialCasing `FB03; FB03; 0046 0066 0069; 0046 0046 0049;` |
| U+0587 `և` | `upper` | U+0535 U+0552 | SpecialCasing `0587; 0587; 0535 0582; 0535 0552;` |
| U+1F88 `ᾈ` | `upper`, `lower`, `category` | U+1F08 U+0399, U+1F80, `Lt` | SpecialCasing `1F88; 1F80; 1F88; 1F08 0399;` |
| U+01C5 `ǅ` | `upper`, `lower`, `is_uppercase`, `is_lowercase` | U+01C4, U+01C6, false, false | UnicodeData `01C5;...;Lt;...;01C4;01C6;01C5`; DerivedCoreProperties lists 01C5 in neither Uppercase nor Lowercase |
| U+03A3 `Σ` | `lower` | U+03C3 `σ`, in every position | UnicodeData `03A3;...;03C3;`; SpecialCasing `03A3; 03C2; 03A3; 03A3; Final_Sigma;` is excluded |
| U+212A `K` | `lower`, `upper` | U+006B `k`, U+212A | UnicodeData `212A;KELVIN SIGN;Lu;...;006B;` |
| U+2167 `Ⅷ` | `lower`, `category`, `is_alphabetic`, `is_letter`, `is_numeric`, `is_uppercase` | U+2177, `Nl`, true, false, true, true | UnicodeData `2167;...;Nl;...;8;N;;;;2177;`; DerivedCoreProperties `2160..2182 ; Alphabetic`, `2160..216F ; Uppercase` |
| U+0345 | `upper`, `category`, `is_alphabetic`, `is_letter`, `is_lowercase` | U+0399, `Mn`, true, false, true | UnicodeData `0345;...;Mn;...;0399;;0399`; DerivedCoreProperties `0345 ; Alphabetic`, `0345 ; Lowercase` |
| U+00AA `ª` | `category`, `is_lowercase`, `upper` | `Lo`, true, `ª` | UnicodeData `00AA;...;Lo;...;;;;`; DerivedCoreProperties `00AA ; Lowercase` |
| U+0660 `٠` | `is_decimal`, `is_alphabetic`, `is_alphanumeric` | true, false, true | UnicodeData `0660;ARABIC-INDIC DIGIT ZERO;Nd;...` |
| U+00BD `½` | `is_numeric`, `is_decimal` | true, false | UnicodeData `00BD;...;No;...;1/2;...` |
| U+000B | `is_whitespace`, `is_ascii_whitespace`, `category` | true, false, `Cc` | PropList `0009..000D ; White_Space` |
| U+0085 | `is_whitespace` | true | PropList `0085 ; White_Space` |
| U+10400 `𐐀` | `lower` | U+10428 | UnicodeData `10400;DESERET CAPITAL LETTER LONG I;Lu;...;10428;` |
| U+4E00 `一`, U+F0000, U+FFFF | `category` | `Lo`, `Co`, `Cn` | UnicodeData `<CJK Ideograph, First>` and `<Plane 15 Private Use, First>` ranges; U+FFFF is not listed |
| U+A7D3 `ꟓ` | `upper` | U+A7D2 | UnicodeData `A7D3;LATIN SMALL LETTER DOUBLE THORN;Ll;...;A7D2;;A7D2` (no mapping before 17.0) |
| U+0295 `ʕ` | `category`, `is_lowercase` | `Lo`, false | UnicodeData `0295;...;Lo;...` (`Ll` before 17.0) |
| U+0277 `ɷ` | `upper` | U+A7DD | UnicodeData `0277;LATIN SMALL LETTER CLOSED OMEGA;Ll;...;A7DD;;A7DD` (no mapping in 17.0) |
| U+A7DD | `lower`, `category` | U+0277, `Lu` | UnicodeData `A7DD;LATIN CAPITAL LETTER CLOSED OMEGA;Lu;...;0277;` (unassigned in 17.0) |
| U+1DF95 | `upper`, `lower` | `SS`, U+1DF95 | SpecialCasing `1DF95; 1DF95; 0053 0073; 0053 0053;` (unassigned in 17.0) |

The last five rows separate 18.0 from earlier versions. 17.0 is the version of Go's `unicode`
package and of the Rust toolchains this repository pins, so a body that answers from host tables
fails the last three, and one built from 16.0 data fails all five.

| call | result |
|---|---|
| `string.to_upper("Straße")` | `"STRASSE"`, 7 bytes from 7 |
| `string.to_lower("İ")` | `"i\u{307}"`, 3 bytes from 2 |
| `string.to_upper("ΐ")` | `"\u{399}\u{308}\u{301}"`, 6 bytes from 2 |
| `string.to_lower("ΣΑΣ")` | `"σασ"` |
| `string.to_lower("Ⅷ ǅ")` | `"ⅷ ǆ"` |
| `string.to_upper("")` | `""`, admitting nothing |

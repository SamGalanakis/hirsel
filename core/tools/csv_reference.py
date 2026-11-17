#!/usr/bin/env python3
"""Print the C2 reader and C3 writer oracles for std.csv with CPython's csv."""

import csv
import io
import re


READ_INPUTS = [
    "",
    "\n",
    "\r\n",
    "\n\r\n\n",
    ",",
    ",\n",
    ",,\r\n",
    '""',
    '""\n',
    '"",\n',
    ',""\r\n',
    '"",""',
    '""""',
    '""""""""\n',
    'a,"b,c",d\r\n',
    '"a\rb","a\nb","a\r\nb"\n',
    'a,b\r\n\n"c","d""e"\nx,y',
    'a,\n"b",',
    'a, b,  c\n',
    '#comment,"#quoted"\n',
    '\ufeffa,b\n',
    'é,"λ雪","😀"\r\n',
    '" trailing space ", leading\n',
    '\x00,"a\x00b"\n',
    '"first\nsecond\nthird"\n',
    'x,"unterminated',
    '"',
    '"unclosed\r\nnext line',
    '"closed"x',
    '"closed" x,other\n',
]

WRITE_INPUTS = {
    ",": [
        [],
        [""],
        ["", ""],
        ['"'],
        ["\r\n"],
        ["a b", " a", "\\."],
        ["a,b"],
        ["bare\rreturn"],
        ["bare\nline"],
        ["\r"],
        ["\n"],
        ["", "a", ""],
        ["a", "", ""],
        ["", "", "a"],
        ["x", "y", "z"],
        ["a", "b,c", "d"],
        ['a"b'],
        ['"a"b"'],
        ["\t", ";", "|"],
        ["  ", "\tleading", "trailing "],
        ["#comment", "\ufeffBOM"],
        ["é", "λ雪", "😀"],
        ["é,λ", "雪\r😀"],
        ["\x00", "a\x00b"],
        ["x" * 1024],
        ["quote before end\""],
    ],
    ";": [
        [],
        [""],
        [";", "a;b", "a,b"],
        ["a", "", "b"],
        ["\r\n", 'a"b'],
        [" é", '雪;"'],
        ["\t", ""],
    ],
    "\t": [
        [],
        [""],
        ["\t", "a\tb"],
        ["a,b", "a;b", "a|b"],
        ["", ""],
        ["\r", "\n", "\r\n"],
        [" 😀", "\\.", '"'],
    ],
}

# Each token is a quoted field or an unquoted run up to a delimiter or line
# end. The fallback consumes suffix bytes after a closing quote so malformed
# C2 rows can reach Python, while counting any quote in a suffix as bare.
QUOTED = re.compile(r'"(?:[^"]|"")*(?:"(?!")|$)')


def assert_reader_domain(text):
    """Exclude bare quotes and CR outside quotes unless followed by LF."""
    at = 0
    start = True
    while at < len(text):
        byte = text[at]
        if start and byte == '"':
            match = QUOTED.match(text, at)
            assert match is not None, repr(text)
            at = match.end()
            start = False
            continue
        assert byte != '"', repr(text)
        if byte == "\r":
            assert text[at:at + 2] == "\r\n", repr(text)
        start = byte in ",\n"
        at += 1


def main():
    for text in READ_INPUTS:
        assert_reader_domain(text)
        try:
            rows = list(csv.reader(io.StringIO(text, newline=""), strict=True))
        except csv.Error:
            rows = "ERROR"
        print(repr(text), "=>", rows)
    for delimiter, rows in WRITE_INPUTS.items():
        for ending in ("\n", "\r\n"):
            out = io.StringIO(newline="")
            csv.writer(out, delimiter=delimiter, lineterminator=ending).writerows(rows)
            print(f"delimiter={delimiter!r} lineterminator={ending!r} => {out.getvalue()!r}")


if __name__ == "__main__":
    main()

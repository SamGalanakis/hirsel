#!/usr/bin/env python3
"""Print the RFC 3986 percent-encoding oracle, in byte order, for each set."""
from urllib.parse import quote

SUB_DELIMS = "!$&'()*+,;="
SEGMENT = SUB_DELIMS + ":@"
PATH = SEGMENT + "/"
QUERY = PATH + "?"
SETS = {
    "Segment": SEGMENT,
    "Path": PATH,
    "Query": QUERY,
    "QueryPart": QUERY.translate(str.maketrans("", "", "&=+")),
    "UserInfo": SUB_DELIMS + ":",
}


def main():
    # Every set escapes |, so it can separate consecutive byte spellings.
    for name, safe in SETS.items():
        print(f"{name}: {'|'.join(quote(bytes([b]), safe=safe) for b in range(256))}")


if __name__ == "__main__":
    main()

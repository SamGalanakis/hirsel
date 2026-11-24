# XML

`xml.reader` takes an owned byte buffer. Pull one event at a time with
`xml.next`, through the final `None`, to check the entire document. Each
event owns its values. The reader retains the source bytes and open namespace
scopes. It does not build a tree or read from a host stream. An empty element
produces both `Start` and `End`.

`xml.parse` returns a document node table with parent, child and sibling
indices. `xml.root` gives the document element index. Comments and processing
instructions can also appear at document level. Adjacent character nodes can
come from different source constructs, including CDATA sections.

`xml.write` serializes a parsed document as UTF-8 and escapes text and
attribute values. `xml.canonical` uses the XML test suite's James Clark
canonical form, with sorted attributes, no comments and explicit end tags.

The reader handles XML 1.0 Fifth Edition and Namespaces 1.0. It recognizes
UTF-8, UTF-16, declared UTF-16BE/LE, ISO-8859-1 and US-ASCII. It applies XML
1.0 processing rules to declarations with a version in the form `1.x`.
`DtdForbidden` covers every document type declaration. Entity handling is
limited to character references and the five predefined entities. The
package uses no host calls or FFI.

`xml.defaults` limits the source to one MiB, nesting to 128 elements,
attributes to 128 per element, and names to 1,024 UTF-8 bytes. The reader's
step budget counts consumed input bytes and namespace lookup and comparison
work over the lifetime of the reader. The writer uses a Hirsel step bound and
applies the byte limit to emitted bytes. Both APIs return typed limits.

## Conformance

Run `python3 core/programs/xml/xmlconf.py /tmp/ref-xmlconf/xmlconf --inventory skipped.tsv`
with `hirsel` on PATH, or pass its path with `--hirsel`. The script generates a
temporary Hirsel law and checks that law under recording and replay.

Use the [W3C XML Conformance Test Suite 20130923 archive](https://www.w3.org/XML/Test/xmlts20130923.tar.gz).
The [suite process](https://www.w3.org/XML/Test/XMLConformanceTS-Process-20031210.html#ownership)
specifies the W3C Software License for contributions. The archive also contains
older tests with their own notices, including James Clark's redistribution
terms in `xmltest/readme.html`. The law reads the archive from a scratch
directory. The checked-in contract cases are original, under this repository's
license.

Every selected negative must produce a typed XML refusal. The script counts
negative cases containing a DTD so that DTD policy refusals can be distinguished
from syntax coverage. The archive's eligible valid cases have DTDs. The law
removes declaration-only internal subsets that contain only element declarations
and CDATA attribute declarations without defaults. It compares the result with
the archive's unmodified canonical output wherever the catalogue provides one.
The skip inventory records a reason and the original ID for every excluded case.

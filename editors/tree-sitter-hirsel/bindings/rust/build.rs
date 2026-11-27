fn main() {
    cc::Build::new()
        .std("c11")
        .include("src")
        .file("src/parser.c")
        .file("src/scanner.c")
        .compile("tree-sitter-hirsel");

    cc::Build::new()
        .std("c11")
        .include("data/src")
        .file("data/src/parser.c")
        .file("data/src/scanner.c")
        .compile("tree-sitter-hirsel-data");

    println!("cargo:rerun-if-changed=data/src");
    println!("cargo:rerun-if-changed=src");
}

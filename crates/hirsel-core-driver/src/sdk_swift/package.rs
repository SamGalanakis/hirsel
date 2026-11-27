use super::{Error, Path, Slice, entries, fs, upper_camel};
use std::collections::BTreeMap;
use std::fmt::Write;

pub(super) fn write(
    out: &Path,
    name: &str,
    projection: &[u8],
    slices: &[Slice],
) -> Result<(), Error> {
    let mut module = upper_camel(name);
    if !module.starts_with(|c: char| c.is_ascii_alphabetic()) {
        module.insert_str(0, "Hirsel");
    }
    let core = format!("{module}Core");
    let bundle = format!("{core}.xcframework");
    let archive = format!("lib{name}.a");
    let (codec, entries) = entries::render(projection, slices)?;
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let staging = tempfile::tempdir_in(parent)?;
    let root = staging.path();
    let source = root.join("Sources").join(&module);
    let tests = root.join("Tests").join(format!("{module}Tests"));
    let manifests = root.join("manifests");
    for dir in [&source, &tests, &manifests] {
        fs::create_dir_all(dir)?;
    }
    fs::write(root.join("projection.json"), projection)?;
    fs::write(
        root.join("Package.swift"),
        package(&module, &core, &bundle, slices),
    )?;
    fs::write(source.join("Codec.swift"), codec)?;
    fs::write(
        source.join("Entries.swift"),
        format!("import {core}\n{entries}\nlet embedPackage = {name:?}\n"),
    )?;
    fs::write(
        source.join("Runtime.swift"),
        format!("import {core}\n{}", include_str!("Runtime.swift")),
    )?;
    for (file, body) in [
        ("CodecVectors.swift", include_str!("CodecVectors.swift")),
        (
            "CancellationRaces.swift",
            include_str!("CancellationRaces.swift"),
        ),
    ] {
        fs::write(
            tests.join(file),
            format!("import {core}\n@testable import {module}\n{body}"),
        )?;
    }
    let xcframework = root.join(bundle);
    for slice in slices {
        let row = slice.apple.xcframework_slice;
        let directory = xcframework.join(row);
        let headers = directory.join("Headers");
        fs::create_dir_all(&headers)?;
        fs::copy(&slice.archive, directory.join(&archive))?;
        for header in ["hirsel_embed.h", "hirsel_product.h"] {
            fs::copy(slice.archive.with_file_name(header), headers.join(header))?;
        }
        fs::write(
            headers.join("module.modulemap"),
            format!(
                "module {core} {{\n    header \"hirsel_embed.h\"\n    header \"hirsel_product.h\"\n    link framework \"CoreFoundation\"\n    link framework \"Security\"\n    export *\n}}\n"
            ),
        )?;
        fs::write(manifests.join(format!("{row}.json")), &slice.manifest)?;
    }
    fs::write(xcframework.join("Info.plist"), plist(&archive, slices))?;
    fs::rename(root, out)?;
    Ok(())
}

fn package(module: &str, core: &str, bundle: &str, slices: &[Slice]) -> String {
    let platforms = slices
        .iter()
        .map(|s| (s.apple.swiftpm_platform, s.apple.floor))
        .collect::<BTreeMap<_, _>>()
        .iter()
        .map(|(platform, floor)| format!(".{platform}(\"{floor}\")"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        r#"// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "{module}",
    platforms: [{platforms}],
    products: [.library(name: "{module}", targets: ["{module}"])],
    targets: [
        .binaryTarget(name: "{core}", path: "{bundle}"),
        .target(name: "{module}", dependencies: ["{core}"]),
        .testTarget(name: "{module}Tests", dependencies: ["{module}", "{core}"]),
    ]
)
"#
    )
}

fn plist(archive: &str, slices: &[Slice]) -> String {
    let mut rows: Vec<_> = slices.iter().map(|slice| slice.apple).collect();
    rows.sort_by_key(|apple| apple.xcframework_slice);
    let mut xml = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>AvailableLibraries</key>
    <array>
"#,
    );
    for apple in rows {
        writeln!(
            xml,
            r#"        <dict>
            <key>HeadersPath</key>
            <string>Headers</string>
            <key>LibraryIdentifier</key>
            <string>{}</string>
            <key>LibraryPath</key>
            <string>{archive}</string>
            <key>SupportedArchitectures</key>
            <array>
                <string>arm64</string>
            </array>
            <key>SupportedPlatform</key>
            <string>{}</string>"#,
            apple.xcframework_slice, apple.family
        )
        .unwrap();
        if !apple.llvm_environment.is_empty() {
            writeln!(
                xml,
                "            <key>SupportedPlatformVariant</key>\n            <string>{}</string>",
                apple.llvm_environment
            )
            .unwrap();
        }
        xml.push_str("        </dict>\n");
    }
    xml.push_str(
        r#"    </array>
    <key>CFBundlePackageType</key>
    <string>XFWK</string>
    <key>XCFrameworkFormatVersion</key>
    <string>1.0</string>
</dict>
</plist>
"#,
    );
    xml
}

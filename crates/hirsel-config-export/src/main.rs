//! Host bootstrap exporter. The caller pins the accepted root before opening the document.
mod vector;
hirsel_guard::install!();
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let usage = "usage: hirsel-config-export MODULE.ROOT FILE";
    let root = args.next().ok_or(usage)?;
    if root == "--vector-package" {
        let paths: Vec<_> = args.map(std::path::PathBuf::from).collect();
        let [package, vector, directory] = paths.as_slice() else {
            return Err("expected PACKAGE VECTOR DIRECTORY".into());
        };
        print!("{}", vector::compose(package, vector, directory)?);
        return Ok(());
    }
    let path = args.next().ok_or(usage)?;
    if args.next().is_some() {
        return Err(usage.into());
    }
    if root == "--auto" {
        return hirsel_config_schema::export(
            std::path::Path::new(&path),
            &hirsel_config_schema::catalog(),
            std::io::stdout().lock(),
        );
    }
    let schema =
        hirsel_config_schema::schema(root.to_str().ok_or(usage)?).ok_or("unknown schema root")?;
    hirsel_config_schema::export(
        std::path::Path::new(&path),
        &[schema],
        std::io::stdout().lock(),
    )
}

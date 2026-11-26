//! `hirsel coverage merge` and `hirsel coverage diff` (`docs/coverage.md`,
//! "The coverage tool"): saved profiles, and raw files read against the site
//! maps their builds wrote, merged and reported with no compiler state.
use super::{
    Profile, Refusal, SiteMap, Targets,
    report::{Diff, Others},
};
use crate::cli::{Invocation, OptionId};
use crate::request::Outcome;
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

type Error = Box<dyn std::error::Error>;

/// Run `coverage merge` or `coverage diff`. A usage error is an `Err` (exit
/// 2); an input the tool refuses prints the refusal and exits 1.
pub(crate) fn run(invocation: Invocation) -> Result<Outcome, Error> {
    let command = invocation.command.name;
    let mut inputs: Vec<String> = invocation.positional.iter().cloned().collect();
    inputs.extend(invocation.arguments.iter().cloned());
    let (mut maps, mut base, mut targets, mut json, mut output, mut test) =
        (Vec::new(), None, None, false, None, None);
    for (id, given) in invocation.options {
        let value = || PathBuf::from(given.clone().expect("coverage option takes a value"));
        match id {
            OptionId::Json => json = true,
            OptionId::CoverageMap => maps.push(value()),
            OptionId::CoverageBase => base = Some(value()),
            OptionId::CoverageTargets => targets = Some(value()),
            OptionId::Output => output = Some(value()),
            OptionId::TestExact => test = given.clone(),
            _ => return Err(format!("{command} does not take that option").into()),
        }
    }
    let done = match command {
        "coverage diff" => match (inputs.as_slice(), test.as_deref()) {
            ([profile], Some(test)) => others(Path::new(profile), test, json),
            (_, Some(_)) => return Err("coverage diff --test requires one PROFILE".into()),
            ([base, new], None) => diff(Path::new(base), Path::new(new), json),
            _ => return Err("coverage diff requires BASE and NEW profiles".into()),
        },
        "coverage merge" => {
            if inputs.is_empty() {
                return Err("coverage merge requires a profile or raw file".into());
            }
            merge(
                &inputs,
                &maps,
                base.as_deref(),
                targets.as_deref(),
                json,
                output.as_deref(),
            )
        }
        _ => return Err("coverage requires merge or diff".into()),
    };
    Ok(match done {
        Ok(()) => Outcome::Exit(0),
        Err(Refusal(why)) => {
            eprintln!("coverage: {why}");
            Outcome::Exit(1)
        }
    })
}

fn diff(base: &Path, new: &Path, json: bool) -> super::Result<()> {
    let diff = Diff::of(&profile(base)?, &profile(new)?);
    if json {
        println!("{}", diff.json());
    } else {
        println!("{}", diff.line());
        for key in &diff.gained {
            println!("  + {key}");
        }
        for key in &diff.lost {
            println!("  - {key}");
        }
        for function in &diff.changed_functions {
            println!("  changed {function}");
        }
    }
    Ok(())
}

/// `coverage diff PROFILE --test TEST`: the sites only the profile's other
/// tests reached (`+`), and only TEST reached (`-`), each with its shape.
fn others(path: &Path, test: &str, json: bool) -> super::Result<()> {
    let profile = profile(path)?;
    let others = Others::of(&profile, test)
        .ok_or_else(|| at(path, Refusal(format!("the profile holds no run of {test}"))))?;
    if json {
        println!("{}", others.json());
    } else {
        println!("{}", others.line());
        let row = |sign: char, key: &String| {
            let shape = profile.sites[key].shape;
            match shape {
                Some(shape) => println!("  {sign} {key}  {}", shape.name()),
                None => println!("  {sign} {key}"),
            }
        };
        others.others_only.iter().for_each(|key| row('+', key));
        others.test_only.iter().for_each(|key| row('-', key));
    }
    Ok(())
}

/// The files `coverage merge` reads: a directory stands for every file in it,
/// in name order, and `@LIST` for the paths LIST holds, one per line, so a
/// campaign's thousands of raw files need not each be an argument.
fn expand(inputs: &[String]) -> super::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for input in inputs {
        if let Some(list) = input.strip_prefix('@') {
            let list = Path::new(list);
            let text = String::from_utf8(read(list)?)
                .map_err(|e| Refusal(format!("{}: {e}", list.display())))?;
            files.extend(
                text.lines()
                    .filter(|line| !line.is_empty())
                    .map(PathBuf::from),
            );
        } else if Path::new(input).is_dir() {
            let mut entries = std::fs::read_dir(input)
                .and_then(|dir| {
                    dir.map(|entry| Ok(entry?.path()))
                        .collect::<std::io::Result<Vec<_>>>()
                })
                .map_err(|e| Refusal(format!("{input}: {e}")))?;
            entries.retain(|path| !path.is_dir());
            entries.sort();
            files.extend(entries);
        } else {
            files.push(PathBuf::from(input));
        }
    }
    if files.is_empty() {
        return Err(Refusal(format!("{} names no input file", inputs.join(" "))));
    }
    Ok(files)
}

fn merge(
    inputs: &[String],
    maps: &[PathBuf],
    base: Option<&Path>,
    targets: Option<&Path>,
    json: bool,
    output: Option<&Path>,
) -> super::Result<()> {
    let maps = maps
        .iter()
        .map(|path| {
            Ok((
                path.clone(),
                SiteMap::from_json(&document(path)?).map_err(|r| at(path, r))?,
            ))
        })
        .collect::<super::Result<Vec<_>>>()?;
    let mut merged: Option<Profile> = None;
    for path in &expand(inputs)? {
        let bytes = read(path)?;
        let profile = if bytes.starts_with(b"HIRSELCV") {
            raw(path, &bytes, &maps)?
        } else {
            let document = serde_json::from_slice(&bytes)
                .map_err(|e| Refusal(format!("{}: {e}", path.display())))?;
            Profile::from_json(&document).map_err(|r| at(path, r))?
        };
        match &mut merged {
            None => merged = Some(profile),
            Some(merged) => merged.merge(profile).map_err(|r| at(path, r))?,
        }
    }
    let merged = merged.expect("coverage merge has an input");
    let base = base.map(profile).transpose()?;
    let targets = targets
        .map(|path| {
            Targets::read(&String::from_utf8_lossy(&read(path)?))
                .map_err(|why| Refusal(format!("{}: {why}", path.display())))
        })
        .transpose()?;
    let report = merged.report(targets.as_ref(), base.as_ref());
    if let Some(output) = output {
        std::fs::write(output, format!("{}\n", report.document))
            .map_err(|e| Refusal(format!("{}: {e}", output.display())))?;
    }
    if json {
        println!("{}", report.document);
    } else {
        print!("{}", report.text);
    }
    Ok(())
}

/// The profile of one raw file. With one map, the file pairs with it;
/// otherwise with the one map whose name, less `.covmap`, begins the raw
/// file's name (a test's executable and its runs share that name).
fn raw(path: &Path, bytes: &[u8], maps: &[(PathBuf, SiteMap)]) -> super::Result<Profile> {
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let paired: Vec<&SiteMap> = match maps {
        [] => return Err(at(path, Refusal("a raw file needs --map".into()))),
        [(_, map)] => vec![map],
        _ => maps
            .iter()
            .filter(|(map, _)| {
                map.file_name()
                    .and_then(|n| n.to_str())
                    .and_then(|n| n.strip_suffix(".covmap"))
                    .is_some_and(|stem| name.starts_with(stem))
            })
            .map(|(_, map)| map)
            .collect(),
    };
    let [map] = paired.as_slice() else {
        return Err(at(
            path,
            Refusal(format!(
                "{} site maps name this raw file; it needs one",
                paired.len()
            )),
        ));
    };
    let refused = |why: String| at(path, Refusal(why));
    let (seed, passed) = crate::test_run::header(bytes).map_err(refused)?;
    let flags: BTreeMap<String, Vec<u8>> = crate::test_run::records(bytes)
        .map_err(refused)?
        .into_iter()
        .map(|(id, flags)| (id.to_string(), flags))
        .collect();
    // A raw file is one run, so it keeps its run row: the seeds that reached
    // a site come from those rows ("Reached by").
    Ok(map
        .run(flags, seed, passed, true)
        .map_err(|r| at(path, r))?
        .0)
}

fn profile(path: &Path) -> super::Result<Profile> {
    Profile::from_json(&document(path)?).map_err(|r| at(path, r))
}

fn document(path: &Path) -> super::Result<Value> {
    serde_json::from_slice(&read(path)?).map_err(|e| Refusal(format!("{}: {e}", path.display())))
}

fn read(path: &Path) -> super::Result<Vec<u8>> {
    std::fs::read(path).map_err(|e| Refusal(format!("{}: {e}", path.display())))
}

fn at(path: &Path, refusal: Refusal) -> Refusal {
    Refusal(format!("{}: {}", path.display(), refusal.0))
}

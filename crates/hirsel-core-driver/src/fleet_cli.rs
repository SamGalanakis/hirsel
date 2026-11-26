//! CLI rewrite for `hirsel fleet sim` onto the standard native `run` pipeline.
use crate::cli::{Invocation, OptionId as Opt};
use std::path::PathBuf;

pub(crate) fn prepare(
    invocation: &mut Invocation,
) -> Result<&'static str, Box<dyn std::error::Error>> {
    if invocation.command.name != "fleet sim" {
        return Ok(invocation.command.name);
    }
    let mut forwarded = Vec::new();
    let mut kept = Vec::new();
    let mut has_limit = false;
    for (opt, val) in std::mem::take(&mut invocation.options) {
        match opt {
            Opt::FleetHosts => {
                let v = val.unwrap_or_default();
                let n: u64 = v.parse().map_err(|_| "--hosts expects a positive u64")?;
                if n == 0 {
                    return Err("--hosts must be positive".into());
                }
                forwarded.push("--hosts".to_owned());
                forwarded.push(v);
            }
            Opt::SimSeed => {
                let v = val.unwrap_or_default();
                v.parse::<u64>().map_err(|_| "--seed expects a u64")?;
                forwarded.push("--seed".to_owned());
                forwarded.push(v);
            }
            Opt::FleetRounds => {
                let v = val.unwrap_or_default();
                let n: u64 = v.parse().map_err(|_| "--rounds expects a positive u64")?;
                if n == 0 {
                    return Err("--rounds must be positive".into());
                }
                forwarded.push("--rounds".to_owned());
                forwarded.push(v);
            }
            Opt::FleetPlaces => {
                let v = val.unwrap_or_default();
                let n: u64 = v.parse().map_err(|_| "--places expects a positive u64")?;
                if n == 0 {
                    return Err("--places must be positive".into());
                }
                forwarded.push("--places".to_owned());
                forwarded.push(v);
            }
            Opt::FleetPartitions => {
                let v = val.unwrap_or_default();
                let n: u64 = v
                    .parse()
                    .map_err(|_| "--partitions expects a positive u64")?;
                if n == 0 {
                    return Err("--partitions must be positive".into());
                }
                forwarded.push("--partitions".to_owned());
                forwarded.push(v);
            }
            Opt::Json => {
                forwarded.push("--json".to_owned());
            }
            Opt::Limit => {
                has_limit = true;
                kept.push((opt, val));
            }
            other => kept.push((other, val)),
        }
    }
    if !has_limit {
        kept.push((Opt::Limit, Some("536870912".to_owned())));
    }
    kept.push((Opt::Entry, Some("fleet@1/fleet_sim/main".to_owned())));
    for grant in ["Clock", "Tasks", "Stdout", "Stderr"] {
        kept.push((Opt::Grant, Some(grant.to_owned())));
    }
    invocation.options = kept;
    invocation.arguments.extend(forwarded);
    if invocation.positional.is_none() {
        let default_dir = PathBuf::from("core/programs/fleet");
        if default_dir.is_dir() {
            invocation.positional = Some(default_dir.to_string_lossy().into_owned());
        } else {
            return Err(
                "fleet sim: pass the fleet package directory (or run from a repo containing core/programs/fleet)"
                    .into(),
            );
        }
    }
    Ok("run")
}

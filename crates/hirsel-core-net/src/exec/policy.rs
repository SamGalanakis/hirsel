//! The launch request and the policy it pins (EXEC-1 and EXEC-2).
use super::{ArgToken, ExecRules, Form, ProfileRules, valid_key, valid_name};
use ring::digest::{Context, SHA256};
use std::collections::BTreeMap;
use std::path::Path;

pub const GRANT_SYNTAX: &str = "Exec requires --grant 'Exec=FILE NAME[,NAME]'";

#[derive(Clone, Debug, serde::Serialize)]
pub struct Request {
    pub file: String,
    pub names: Vec<String>,
}

impl Request {
    pub fn parse(text: &str) -> Result<Self, &'static str> {
        let (file, names) = text.rsplit_once(' ').ok_or(GRANT_SYNTAX)?;
        let names = names.split(',').map(str::to_owned).collect::<Vec<_>>();
        let mut seen = std::collections::BTreeSet::new();
        if file.is_empty()
            || file.as_bytes().contains(&0)
            || !names
                .iter()
                .all(|name| valid_name(name) && seen.insert(name))
        {
            return Err(GRANT_SYNTAX);
        }
        Ok(Self {
            file: file.to_owned(),
            names,
        })
    }

    pub fn text(&self) -> String {
        format!("{} {}", self.file, self.names.join(","))
    }

    pub fn load(&self) -> Result<Policy, String> {
        self.load_from(Path::new(""))
    }

    pub fn load_from(&self, workdir: &Path) -> Result<Policy, String> {
        let source = std::fs::read_to_string(workdir.join(&self.file)).map_err(|error| {
            format!(
                "Exec[{}]: could not read policy {:?}: {error}",
                self.names.join(","),
                self.file
            )
        })?;
        Policy::parse(&source, &self.names)
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ProfileFile {
    image: String,
    sha256: String,
    argv0: String,
    args: Vec<Vec<String>>,
    env: BTreeMap<String, String>,
    dirs: BTreeMap<String, String>,
}

pub struct Profile {
    pub image: String,
    pub pin: [u8; 32],
    pub argv0: String,
    pub forms: Vec<Form>,
    pub fixed: BTreeMap<String, String>,
    pub caller_keys: Vec<String>,
    pub dirs: BTreeMap<String, String>,
}

fn ascii_hex(s: &str) -> Option<[u8; 32]> {
    let mut bytes = [0u8; 32];
    if s.len() != 64
        || !s
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return None;
    }
    for (at, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[at * 2..at * 2 + 2], 16).ok()?;
    }
    Some(bytes)
}

impl Profile {
    fn parse(raw: toml::Value) -> Result<Self, &'static str> {
        let raw: ProfileFile = raw.try_into().map_err(|_| "malformed profile")?;
        let absolute = |path: &str| Path::new(path).is_absolute() && !path.as_bytes().contains(&0);
        if !absolute(&raw.image) {
            return Err("image path must be absolute and contain no NUL");
        }
        if raw.dirs.is_empty()
            || raw
                .dirs
                .iter()
                .any(|(label, dir)| !valid_name(label) || !absolute(dir))
        {
            return Err("cwd labels must name absolute paths without NUL");
        }
        if raw.argv0.as_bytes().contains(&0) {
            return Err("argv0 contains NUL");
        }
        let pin = ascii_hex(&raw.sha256).ok_or("sha256 must be 64 lowercase hex digits")?;
        let forms = raw
            .args
            .into_iter()
            .map(|tokens| {
                Form::new(
                    tokens
                        .into_iter()
                        .map(|token| match token.as_str() {
                            "{any}" => ArgToken::Any,
                            "{rest}" => ArgToken::Rest,
                            _ => ArgToken::Lit(token.into_bytes()),
                        })
                        .collect(),
                )
                .ok_or("argument form contains NUL or nonfinal {rest}")
            })
            .collect::<Result<_, _>>()?;
        let mut fixed = BTreeMap::new();
        let mut caller_keys = Vec::new();
        for (key, value) in raw.env {
            if !valid_key(&key) || value.as_bytes().contains(&0) {
                return Err("invalid environment key or NUL in environment value");
            }
            if value == "{any}" {
                caller_keys.push(key);
            } else {
                fixed.insert(key, value);
            }
        }
        Ok(Self {
            image: raw.image,
            pin,
            argv0: raw.argv0,
            forms,
            fixed,
            caller_keys,
            dirs: raw.dirs,
        })
    }

    fn encode(&self, digest: &mut Context) -> Option<()> {
        text(digest, self.image.as_bytes())?;
        digest.update(&self.pin);
        text(digest, self.argv0.as_bytes())?;
        count(digest, self.forms.len())?;
        for form in &self.forms {
            count(digest, form.len())?;
            for token in form.tokens() {
                match token {
                    ArgToken::Lit(literal) => {
                        digest.update(&[0]);
                        text(digest, literal)?;
                    }
                    ArgToken::Any => digest.update(&[1]),
                    ArgToken::Rest => digest.update(&[2]),
                }
            }
        }
        map(digest, &self.fixed)?;
        count(digest, self.caller_keys.len())?;
        for key in &self.caller_keys {
            text(digest, key.as_bytes())?;
        }
        map(digest, &self.dirs)
    }
}

fn count(digest: &mut Context, count: usize) -> Option<()> {
    digest.update(&u32::try_from(count).ok()?.to_le_bytes());
    Some(())
}

fn text(digest: &mut Context, value: &[u8]) -> Option<()> {
    count(digest, value.len())?;
    digest.update(value);
    Some(())
}

fn map(digest: &mut Context, rows: &BTreeMap<String, String>) -> Option<()> {
    count(digest, rows.len())?;
    for (key, value) in rows {
        text(digest, key.as_bytes())?;
        text(digest, value.as_bytes())?;
    }
    Some(())
}

pub struct Policy {
    pub profiles: BTreeMap<String, Profile>,
    pub digest: [u8; 32],
}

impl Policy {
    fn parse(source: &str, names: &[String]) -> Result<Self, String> {
        let table = source
            .parse::<toml::Table>()
            .map_err(|_| format!("Exec[{}]: malformed TOML policy", names.join(",")))?;
        let mut profiles = BTreeMap::new();
        for (name, raw) in table {
            let profile = Profile::parse(raw).map_err(|why| format!("Exec[{name:?}]: {why}"))?;
            if !valid_name(&name) {
                return Err(format!("Exec[{name:?}]: invalid profile name"));
            }
            if names.contains(&name) {
                profiles.insert(name, profile);
            }
        }
        let mut digest = Context::new(&SHA256);
        digest.update(b"hirsel.exec-policy.v1");
        count(&mut digest, profiles.len()).ok_or("too many Exec profiles")?;
        for name in names {
            if !profiles.contains_key(name) {
                return Err(format!("Exec[{name:?}]: unknown profile"));
            }
        }
        for (name, profile) in &profiles {
            text(&mut digest, name.as_bytes())
                .and_then(|()| profile.encode(&mut digest))
                .ok_or_else(|| format!("Exec[{name:?}]: policy field exceeds u32"))?;
        }
        let digest = digest.finish().as_ref().try_into().unwrap();
        Ok(Self { profiles, digest })
    }

    pub fn rules(&self) -> ExecRules {
        ExecRules::host(
            self.profiles
                .iter()
                .map(|(name, profile)| {
                    (
                        name.clone(),
                        ProfileRules::host(
                            profile.forms.clone(),
                            profile.caller_keys.iter().cloned().collect(),
                            profile.dirs.keys().cloned().collect(),
                            profile.fixed.keys().cloned().collect(),
                        ),
                    )
                })
                .collect(),
        )
    }
}

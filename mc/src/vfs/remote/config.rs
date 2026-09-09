//! Deliberately non-executing subset of OpenSSH configuration.
use super::*;
use std::collections::HashMap;

#[derive(Clone, Debug)]
pub(super) struct Config {
    pub endpoint: Endpoint,
    pub keys: Vec<PathBuf>,
    pub identities_only: bool,
    pub allow_unknown: bool,
    pub jump: Option<String>,
}
impl Config {
    pub fn resolve(
        mut endpoint: Endpoint,
        user_set: bool,
        port_set: bool,
        home: &Path,
    ) -> Result<Self> {
        let original = endpoint.host.clone();
        let mut values = HashMap::new();
        let mut identities = vec![];
        let file = home.join(".ssh/config");
        if file.exists() {
            parse(
                &file,
                &original,
                home,
                &mut true,
                &mut values,
                &mut identities,
                0,
            )?;
        }
        if let Some(v) = values.get("hostname") {
            endpoint.host = v.clone();
        }
        if !user_set && let Some(v) = values.get("user") {
            endpoint.user = v.clone();
        }
        if !port_set && let Some(v) = values.get("port") {
            endpoint.port = v.parse().context("Invalid SSH config Port")?;
        }
        safe(&endpoint.host)?;
        safe(&endpoint.user)?;
        ensure!(endpoint.port > 0, "Invalid SSH port");
        let expand = |v: &str| -> Result<String> {
            let mut out = String::new();
            let mut chars = v.chars();
            while let Some(c) = chars.next() {
                if c != '%' {
                    out.push(c);
                    continue;
                }
                out.push_str(&match chars.next() {
                    Some('%') => "%".into(),
                    Some('d') => home.to_string_lossy().into_owned(),
                    Some('h') => endpoint.host.clone(),
                    Some('n') => original.clone(),
                    Some('r') => endpoint.user.clone(),
                    Some('p') => endpoint.port.to_string(),
                    _ => bail!("Unsupported SSH config token"),
                });
            }
            Ok(out)
        };
        let keys = if identities.is_empty() {
            vec![home.join(".ssh/id_ed25519"), home.join(".ssh/id_rsa")]
        } else {
            identities
                .iter()
                .filter(|v| v.as_str() != "none")
                .map(|v| expand(v).map(|v| home_path(&v, home)))
                .collect::<Result<_>>()?
        };
        let jump = values
            .get("proxyjump")
            .filter(|v| v.as_str() != "none")
            .map(|v| expand(v))
            .transpose()?;
        Ok(Self {
            endpoint,
            keys,
            jump,
            allow_unknown: !values
                .get("stricthostkeychecking")
                .is_some_and(|v| v == "yes"),
            identities_only: values.get("identitiesonly").is_some_and(|v| v == "yes"),
        })
    }
}
fn home_path(value: &str, home: &Path) -> PathBuf {
    if let Some(v) = value.strip_prefix("~/") {
        home.join(v)
    } else {
        PathBuf::from(value)
    }
}
fn parse(
    file: &Path,
    host: &str,
    home: &Path,
    active: &mut bool,
    values: &mut HashMap<String, String>,
    keys: &mut Vec<String>,
    depth: usize,
) -> Result<()> {
    ensure!(depth < 8, "SSH config Include nesting exceeds 8");
    let content = std::fs::read_to_string(file)?;
    ensure!(content.len() <= 1024 * 1024, "SSH config exceeds 1 MiB");
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let split = line.find(|c: char| c.is_whitespace() || c == '=');
        let Some(split) = split else {
            continue;
        };
        let key = line[..split].to_ascii_lowercase();
        let args = shell_words::split(
            line[split..].trim_start_matches(|c: char| c.is_whitespace() || c == '='),
        )?;
        if key == "host" {
            let mut yes = false;
            let mut no = false;
            for pattern in args {
                if let Some(p) = pattern.strip_prefix('!') {
                    no |= crate::app::wildcard(&p.to_lowercase(), host);
                } else {
                    yes |= crate::app::wildcard(&pattern.to_lowercase(), host);
                }
            }
            *active = yes && !no;
        } else if key == "match" {
            bail!("SSH config Match blocks are unsupported; use Host blocks for mc connections");
        } else if *active {
            if key == "include" {
                for pattern in args {
                    let p = home_path(&pattern, home);
                    let p = if p.is_absolute() {
                        p
                    } else {
                        home.join(".ssh").join(p)
                    };
                    // Wildcards in the filename, sorted just like OpenSSH Include.
                    let mut files = vec![];
                    if let Some(parent) = p.parent()
                        && parent.is_dir()
                    {
                        let pattern = p.file_name().context("Invalid Include")?.to_string_lossy();
                        for e in std::fs::read_dir(parent)? {
                            let e = e?;
                            if crate::app::wildcard(&pattern, &e.file_name().to_string_lossy()) {
                                files.push(e.path());
                            }
                        }
                    }
                    files.sort();
                    for f in files {
                        parse(&f, host, home, active, values, keys, depth + 1)?;
                    }
                }
            } else if key == "identityfile" {
                if let Some(v) = args.first()
                    && !keys.contains(v)
                {
                    keys.push(v.clone());
                }
            } else if [
                "hostname",
                "user",
                "port",
                "identitiesonly",
                "proxyjump",
                "stricthostkeychecking",
            ]
            .contains(&key.as_str())
            {
                if let Some(v) = args.first() {
                    values.entry(key).or_insert_with(|| v.clone());
                }
            } else if [
                "proxycommand",
                "hostkeyalias",
                "userknownhostsfile",
                "certificatefile",
            ]
            .contains(&key.as_str())
            {
                bail!(
                    "SSH config {key} is unsupported; refusing to ignore connection/security settings"
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aliases_keys_include_and_url_overrides() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".ssh")).unwrap();
        std::fs::write(
            home.path().join(".ssh/config"),
            "Include extra\nHost *\n User fallback\n Port 22\n",
        )
        .unwrap();
        std::fs::write(home.path().join(".ssh/extra"), "Host server !excluded\n HostName 127.0.0.1\n User alice\n Port 2222\n IdentityFile ~/custom\n IdentitiesOnly yes\n ProxyJump bastion\n").unwrap();
        let c = Config::resolve(
            Endpoint::parse("ssh://server/").unwrap(),
            false,
            false,
            home.path(),
        )
        .unwrap();
        assert_eq!(
            (&*c.endpoint.host, &*c.endpoint.user, c.endpoint.port),
            ("127.0.0.1", "alice", 2222)
        );
        assert_eq!(c.keys, vec![home.path().join("custom")]);
        assert!(c.identities_only);
        let c = Config::resolve(
            Endpoint::parse("ssh://bob@server:2223/").unwrap(),
            true,
            true,
            home.path(),
        )
        .unwrap();
        assert_eq!((&*c.endpoint.user, c.endpoint.port), ("bob", 2223));
    }
}

//! Remote mounts share VFS paths/handles with local and archive providers.
mod ftp;
mod ssh;

use super::*;
use anyhow::{Context as _, ensure};
use percent_encoding::{NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use std::net::{TcpStream, ToSocketAddrs};
use url::Url;

const TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Protocol {
    Ftp,
    Sftp,
    Ssh,
}
#[derive(Clone, Debug)]
pub struct Endpoint {
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub user: String,
    pub path: PathBuf,
}
impl Endpoint {
    pub fn parse(value: &str) -> Result<Self> {
        safe(value)?;
        let uri = Url::parse(value).map_err(|_| anyhow::anyhow!("Invalid remote URL"))?;
        let protocol = match uri.scheme() {
            "ftp" => Protocol::Ftp,
            "sftp" => Protocol::Sftp,
            "ssh" => Protocol::Ssh,
            _ => bail!("Use ftp://, sftp://, or ssh://"),
        };
        ensure!(
            uri.password().is_none(),
            "Do not put passwords in URLs; mc prompts privately"
        );
        ensure!(
            uri.query().is_none() && uri.fragment().is_none(),
            "URL queries/fragments are unsupported; percent-encode filename characters"
        );
        let host = match uri.host().context("Remote URL needs a hostname")? {
            url::Host::Domain(h) => h.to_ascii_lowercase(),
            h => h.to_string(),
        };
        let host = host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .to_owned();
        let user = if uri.username().is_empty() {
            if protocol == Protocol::Ftp {
                "anonymous".into()
            } else {
                std::env::var("USER").or_else(|_| std::env::var("USERNAME"))?
            }
        } else {
            decode(uri.username())?
        };
        safe(&user)?;
        safe(&host)?;
        let path = decode(uri.path())?;
        safe(&path)?;
        ensure!(!path.contains('\\'), "Remote paths use forward slashes");
        Ok(Self {
            protocol,
            host,
            user,
            port: uri
                .port()
                .unwrap_or(if protocol == Protocol::Ftp { 21 } else { 22 }),
            path: normalize(Path::new(if path.is_empty() { "/" } else { &path })),
        })
    }
    fn authority(&self) -> String {
        format!(
            "{}@{}:{}",
            utf8_percent_encode(&self.user, NON_ALPHANUMERIC),
            if self.host.contains(':') {
                format!("[{}]", self.host)
            } else {
                self.host.clone()
            },
            self.port
        )
    }
    fn id(&self) -> String {
        // SFTP and the SSH helper operate in the same namespace and share locks.
        format!(
            "{}://{}",
            if self.protocol == Protocol::Ftp {
                "ftp"
            } else {
                "ssh"
            },
            self.authority()
        )
    }
    fn label(&self, path: &Path) -> String {
        let scheme = match self.protocol {
            Protocol::Ftp => "ftp",
            Protocol::Sftp => "sftp",
            Protocol::Ssh => "ssh",
        };
        let encoded = wire_path(path)
            .unwrap_or_else(|_| "/".into())
            .split('/')
            .map(|p| utf8_percent_encode(p, NON_ALPHANUMERIC).to_string())
            .collect::<Vec<_>>()
            .join("/");
        format!("{scheme}://{}{encoded}", self.authority())
    }
}
fn decode(value: &str) -> Result<String> {
    Ok(percent_decode_str(value)
        .decode_utf8()
        .context("Remote names must be UTF-8")?
        .into_owned())
}
fn safe(value: &str) -> Result<()> {
    ensure!(
        !value.chars().any(char::is_control),
        "Control characters are unsupported in remote names"
    );
    Ok(())
}
fn wire_path(path: &Path) -> Result<String> {
    let value = path.to_str().context("Remote names must be UTF-8")?;
    let value = if cfg!(windows) {
        value.replace('\\', "/")
    } else {
        value.into()
    };
    safe(&value)?;
    ensure!(
        !value.contains('\\'),
        "Backslashes are unsupported in remote paths"
    );
    ensure!(value.starts_with('/'), "Remote paths must be absolute");
    Ok(value)
}
fn socket(endpoint: &Endpoint, ctx: &Context) -> Result<TcpStream> {
    ctx.check()?;
    let addresses = (endpoint.host.as_str(), endpoint.port).to_socket_addrs()?;
    let mut last = None;
    for addr in addresses {
        ctx.check()?;
        match TcpStream::connect_timeout(&addr, TIMEOUT) {
            Ok(stream) => {
                stream.set_read_timeout(Some(TIMEOUT))?;
                stream.set_write_timeout(Some(TIMEOUT))?;
                return Ok(stream);
            }
            Err(e) => last = Some(e),
        }
    }
    Err(last
        .map(anyhow::Error::from)
        .unwrap_or_else(|| anyhow::anyhow!("Hostname resolved to no addresses")))
}
pub fn is_url(value: &str) -> bool {
    value.split_once("://").is_some_and(|(scheme, _)| {
        ["ftp", "sftp", "ssh"]
            .iter()
            .any(|s| scheme.eq_ignore_ascii_case(s))
    })
}
pub fn connect(value: &str, ctx: &Context) -> Result<VfsPath> {
    let path = connect_path(value, ctx)?;
    ensure!(
        path.metadata(true, ctx)?.kind == Kind::Directory,
        "Remote location is not a directory"
    );
    Ok(path)
}
/// Establish transport for a location which may be a new copy destination.
pub fn connect_path(value: &str, ctx: &Context) -> Result<VfsPath> {
    let endpoint = Endpoint::parse(value)?;
    let path = endpoint.path.clone();
    let fs: Arc<dyn FileSystem> = match endpoint.protocol {
        Protocol::Ftp => Arc::new(ftp::Ftp::connect(endpoint, ctx)?),
        _ => Arc::new(ssh::Ssh::connect(endpoint, ctx)?),
    };
    Ok(VfsPath::new(fs, path))
}
fn missing() -> anyhow::Error {
    io::Error::new(io::ErrorKind::NotFound, "Remote path does not exist").into()
}
fn remote_name(name: &str) -> Result<()> {
    safe(name)?;
    ensure!(
        !name.is_empty() && !matches!(name, "." | "..") && !name.contains(['/', '\\']),
        "Unsupported remote entry name"
    );
    Ok(())
}

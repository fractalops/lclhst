//! Wire protocol for lclhst streams.
//!
//! Invariant: there is no port field anywhere in this protocol. The serving
//! side dials only the port given on its own command line.

use anyhow::{Context, Result, bail, ensure};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// ALPN for lclhst connections.
pub const ALPN: &[u8] = b"lclhst/0";

/// Protocol version sent in every hello frame.
pub const VERSION: u8 = 0;

/// Status byte: the target reached the local app; raw bytes follow.
pub const STATUS_OK: u8 = 0;
/// Status byte: the target could not connect to the local app.
pub const STATUS_CONNECT_FAILED: u8 = 1;

/// Hello frame sent by the connecting (edge) side: version, then the
/// tunnel name as a length-prefixed DNS label. Informational only.
#[derive(Debug, PartialEq, Eq)]
pub struct Hello {
    pub name: String,
}

/// A valid tunnel name is a lowercase DNS label: `[a-z0-9-]{1,63}`,
/// no leading or trailing hyphen. It becomes `<name>.localhost`.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 63
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

pub async fn write_hello<W: AsyncWrite + Unpin>(w: &mut W, name: &str) -> Result<()> {
    ensure!(valid_name(name), "invalid tunnel name: {name:?}");
    w.write_all(&[VERSION, name.len() as u8]).await?;
    w.write_all(name.as_bytes()).await?;
    Ok(())
}

pub async fn read_hello<R: AsyncRead + Unpin>(r: &mut R) -> Result<Hello> {
    let mut head = [0u8; 2];
    r.read_exact(&mut head)
        .await
        .context("reading hello header")?;
    let [version, len] = head;
    if version != VERSION {
        bail!("peer speaks protocol version {version}, we speak {VERSION}");
    }
    let mut name = vec![0u8; len as usize];
    r.read_exact(&mut name)
        .await
        .context("reading hello name")?;
    let name = String::from_utf8(name).context("hello name is not UTF-8")?;
    ensure!(valid_name(&name), "invalid tunnel name in hello: {name:?}");
    Ok(Hello { name })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn hello_round_trips() {
        let mut buf = Vec::new();
        write_hello(&mut buf, "myapp").await.unwrap();
        let hello = read_hello(&mut buf.as_slice()).await.unwrap();
        assert_eq!(hello.name, "myapp");
    }

    #[tokio::test]
    async fn rejects_wrong_version() {
        let buf = [99u8, 1, b'a'];
        assert!(read_hello(&mut buf.as_slice()).await.is_err());
    }

    #[tokio::test]
    async fn rejects_invalid_name_on_read() {
        // valid version, but name contains uppercase
        let buf = [VERSION, 1, b'A'];
        assert!(read_hello(&mut buf.as_slice()).await.is_err());
    }

    #[test]
    fn name_validation() {
        assert!(valid_name("myapp"));
        assert!(valid_name("my-app-2"));
        assert!(!valid_name(""));
        assert!(!valid_name("-leading"));
        assert!(!valid_name("trailing-"));
        assert!(!valid_name("UPPER"));
        assert!(!valid_name("dots.bad"));
        assert!(!valid_name(&"x".repeat(64)));
        assert!(valid_name(&"x".repeat(63)));
    }
}

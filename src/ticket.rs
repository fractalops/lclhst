//! The lclhst ticket: a tunnel name plus an iroh endpoint ticket.
//! Format: `<name>@<iroh-endpoint-ticket>`. Possession is the capability.

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, bail};
use iroh_tickets::endpoint::EndpointTicket;

use crate::protocol::valid_name;

#[derive(Debug, Clone)]
pub struct Ticket {
    pub name: String,
    pub endpoint: EndpointTicket,
}

impl fmt::Display for Ticket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.name, self.endpoint)
    }
}

impl FromStr for Ticket {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let Some((name, rest)) = s.split_once('@') else {
            bail!("ticket must look like <name>@<endpoint-ticket>");
        };
        if !valid_name(name) {
            bail!("invalid tunnel name in ticket: {name:?}");
        }
        let endpoint = rest.parse().context("invalid endpoint ticket")?;
        Ok(Ticket {
            name: name.to_string(),
            endpoint,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn some_endpoint_ticket() -> iroh_tickets::endpoint::EndpointTicket {
        let ep = iroh::Endpoint::builder(iroh::endpoint::presets::N0)
            .bind()
            .await
            .unwrap();
        iroh_tickets::endpoint::EndpointTicket::new(ep.addr())
    }

    #[tokio::test]
    async fn round_trips_through_string() {
        let t = Ticket {
            name: "myapp".into(),
            endpoint: some_endpoint_ticket().await,
        };
        let parsed: Ticket = t.to_string().parse().unwrap();
        assert_eq!(parsed.name, "myapp");
        assert_eq!(
            parsed.endpoint.endpoint_addr().id,
            t.endpoint.endpoint_addr().id
        );
    }

    #[tokio::test]
    async fn rejects_bad_names_and_shapes() {
        let ep = some_endpoint_ticket().await;
        assert!(format!("UPPER@{ep}").parse::<Ticket>().is_err());
        assert!(format!("no-at-sign{ep}").parse::<Ticket>().is_err());
        assert!("myapp@notaticket".parse::<Ticket>().is_err());
    }
}

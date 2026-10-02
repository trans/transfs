use chrono::{DateTime, SecondsFormat, Utc};
use rand::RngCore;
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::{Error, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Claim {
    Create {
        nonce: String,
        ts: DateTime<Utc>,
    },
    Version {
        hash: String,
        parent: Option<String>,
        ts: DateTime<Utc>,
    },
    Name {
        name: String,
        ts: DateTime<Utc>,
    },
    Tag {
        add: Vec<String>,
        del: Vec<String>,
        ts: DateTime<Utc>,
    },
}

pub fn format_ts(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

impl Claim {
    pub fn mint(ts: DateTime<Utc>) -> Self {
        let mut bytes = [0_u8; 16];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Self::Create {
            nonce: hex::encode(bytes),
            ts,
        }
    }

    pub fn ts(&self) -> DateTime<Utc> {
        match self {
            Self::Create { ts, .. }
            | Self::Version { ts, .. }
            | Self::Name { ts, .. }
            | Self::Tag { ts, .. } => *ts,
        }
    }

    pub fn canonical(&self) -> Vec<u8> {
        let text = match self {
            Self::Create { nonce, ts } => format!("create\x1f{}\x1f{nonce}", format_ts(*ts)),
            Self::Version { hash, parent, ts } => format!(
                "version\x1f{hash}\x1f{}\x1f{}",
                parent.as_deref().unwrap_or(""),
                format_ts(*ts)
            ),
            Self::Name { name, ts } => format!("name\x1f{name}\x1f{}", format_ts(*ts)),
            Self::Tag { add, del, ts } => format!(
                "tag\x1f{}\x1f{}\x1f{}",
                add.join(","),
                del.join(","),
                format_ts(*ts)
            ),
        };
        text.into_bytes()
    }

    pub fn doc_id(&self) -> Option<String> {
        matches!(self, Self::Create { .. }).then(|| hex::encode(Sha256::digest(self.canonical())))
    }

    // Field order matches the existing Crystal JSON-lines codec. IDs hash canonical
    // values, so this order is only for readable disk compatibility.
    pub fn to_json_line(&self) -> String {
        let q = |s: &str| serde_json::to_string(s).expect("strings serialize");
        match self {
            Self::Create { nonce, ts } => format!(
                "{{\"op\":\"create\",\"nonce\":{},\"ts\":{}}}",
                q(nonce),
                q(&format_ts(*ts))
            ),
            Self::Version { hash, parent, ts } => format!(
                "{{\"op\":\"version\",\"hash\":{},\"parent\":{},\"ts\":{}}}",
                q(hash),
                serde_json::to_string(parent).expect("parent serializes"),
                q(&format_ts(*ts))
            ),
            Self::Name { name, ts } => format!(
                "{{\"op\":\"name\",\"name\":{},\"ts\":{}}}",
                q(name),
                q(&format_ts(*ts))
            ),
            Self::Tag { add, del, ts } => {
                let mut line = String::from("{\"op\":\"tag\"");
                if !add.is_empty() {
                    line.push_str(&format!(
                        ",\"add\":{}",
                        serde_json::to_string(add).expect("tags serialize")
                    ));
                }
                if !del.is_empty() {
                    line.push_str(&format!(
                        ",\"del\":{}",
                        serde_json::to_string(del).expect("tags serialize")
                    ));
                }
                line.push_str(&format!(",\"ts\":{}}}", q(&format_ts(*ts))));
                line
            }
        }
    }

    /// Blank lines and syntactically valid unknown ops are ignored, as in Crystal.
    pub fn parse(line: &str) -> Result<Option<Self>> {
        if line.trim().is_empty() {
            return Ok(None);
        }
        let value: Value =
            serde_json::from_str(line).map_err(|e| Error::InvalidClaim(e.to_string()))?;
        let obj = value
            .as_object()
            .ok_or_else(|| Error::InvalidClaim("expected JSON object".into()))?;
        let field = |key: &str| -> Result<&str> {
            obj.get(key)
                .and_then(Value::as_str)
                .ok_or_else(|| Error::InvalidClaim(format!("missing or invalid {key}")))
        };
        let ts = DateTime::parse_from_rfc3339(field("ts")?)
            .map_err(|e| Error::InvalidClaim(e.to_string()))?
            .with_timezone(&Utc);
        let list = |key: &str| -> Result<Vec<String>> {
            match obj.get(key) {
                None => Ok(vec![]),
                Some(Value::Array(values)) => values
                    .iter()
                    .map(|v| {
                        v.as_str()
                            .map(str::to_owned)
                            .ok_or_else(|| Error::InvalidClaim(format!("invalid {key}")))
                    })
                    .collect(),
                Some(_) => Err(Error::InvalidClaim(format!("invalid {key}"))),
            }
        };
        Ok(match field("op")? {
            "create" => Some(Self::Create {
                nonce: field("nonce")?.into(),
                ts,
            }),
            "version" => Some(Self::Version {
                hash: field("hash")?.into(),
                parent: match obj.get("parent") {
                    None | Some(Value::Null) => None,
                    Some(Value::String(s)) => Some(s.clone()),
                    Some(_) => return Err(Error::InvalidClaim("invalid parent".into())),
                },
                ts,
            }),
            "name" => Some(Self::Name {
                name: field("name")?.into(),
                ts,
            }),
            "tag" => Some(Self::Tag {
                add: list("add")?,
                del: list("del")?,
                ts,
            }),
            _ => None,
        })
    }
}

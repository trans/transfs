use chrono::{DateTime, SecondsFormat, Utc};

pub use crate::causal::CausalClaim as Claim;

pub fn format_ts(ts: DateTime<Utc>) -> String {
    ts.to_rfc3339_opts(SecondsFormat::Nanos, true)
}

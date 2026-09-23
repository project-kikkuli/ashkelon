use serde::{Deserialize, Serialize};

use crate::session::SessionKey;
use crate::usage::Usage;
use crate::wire::Wire;

/// One line of the daily call log. Never carries message content.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CallRecord {
    pub ts: String,
    pub call_id: String,
    pub session: SessionKey,
    pub route: String,
    pub wire: Wire,
    pub method: String,
    pub path: String,
    pub status: u16,
    pub model: Option<String>,
    pub stop_reason: Option<String>,
    pub usage: Usage,
    pub tool_calls: Vec<String>,
    pub turn_end: bool,
    pub ttfb_ms: Option<u64>,
    pub total_ms: u64,
    pub request_bytes: u64,
    pub response_bytes: u64,
    /// Names of transforms that changed the request (the upstream saw different bytes than the agent sent).
    pub transforms: Vec<String>,
    /// Pings added to this request.
    pub pings_injected: Vec<String>,
    /// The rule that rejected or cut this call, if any.
    pub rule: Option<String>,
    pub error: Option<String>,
}

//! Replies to a local terminal that sent to a terminal on an added machine.

use crate::machines::Machine;
use crate::remote::{forward, is_peer};
use crate::rpc::RpcError;
use crate::runtime::{Core, State};
use crate::terminals::{Terminal, address, parse_id};
use crate::wire::Conn;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct ReplyGrants {
    confirmed: BTreeMap<String, BTreeSet<u32>>,
    /// One entry per send still in flight.
    #[serde(skip)]
    pending: Vec<(String, u32)>,
}

impl ReplyGrants {
    pub(crate) fn confirmed_is_empty(&self) -> bool {
        self.confirmed.is_empty()
    }
}

struct PendingReply<'a> {
    core: &'a Core,
    sender: u32,
    machine: &'a str,
    recipient: u32,
    confirmed: bool,
}

impl Drop for PendingReply<'_> {
    fn drop(&mut self) {
        let mut st = self.core.lock();
        let Some(t) = st.reg.terminals.get_mut(&self.sender).filter(|t| t.is_live()) else { return };
        let grants = &mut t.reply_grants;
        let Some(index) = grants.pending.iter().position(|(m, id)| m == self.machine && *id == self.recipient) else {
            return;
        };
        let (machine, recipient) = grants.pending.swap_remove(index);
        if self.confirmed && grants.confirmed.entry(machine).or_default().insert(recipient) {
            self.core.save(&st);
        }
    }
}

/// Identify a live terminal on this machine using its last advertised record.
fn mirrored_recipient(st: &State, machine: &Machine, target: &str) -> Option<u32> {
    let terminals = st.mirrors.get(&machine.id)?;
    let prefix = format!("{}/", machine.name);
    if let Some(id) = parse_id(target) {
        return terminals.get(&format!("{prefix}{target}")).filter(|t| t["state"] == "live").map(|_| id);
    }
    terminals
        .iter()
        .filter(|(_, t)| t["state"] == "live" && t["name"].as_str() == Some(target))
        .find_map(|(address, _)| parse_id(address.strip_prefix(&prefix)?))
}

pub(crate) async fn send(
    core: &Core,
    machine: &Machine,
    conn: Option<&Arc<Conn>>,
    mut params: Value,
) -> Result<Value, RpcError> {
    let sender = params["caller"]
        .as_str()
        .and_then(|caller| caller.split_once('/'))
        .filter(|(owner, _)| *owner == core.info.id)
        .and_then(|(_, caller)| parse_id(caller))
        .filter(|_| conn.is_none_or(|conn| !is_peer(core, conn)));

    let mut grant = sender.and_then(|sender| {
        let target = params["terminal"].as_str().filter(|target| !target.contains('/'))?;
        let mut st = core.lock();
        let recipient = mirrored_recipient(&st, machine, target)?;
        let terminal = st.reg.terminals.get_mut(&sender).filter(|t| t.is_live())?;
        terminal.reply_grants.pending.push((machine.id.clone(), recipient));
        Some(PendingReply { core, sender, machine: &machine.id, recipient, confirmed: false })
    });
    if let Some(grant) = &grant {
        // Use the same identity for delivery and authorization even if the name changes hands.
        params["terminal"] = address(grant.recipient).into();
    }
    let reply = forward(core, machine, "terminal.send", params).await;
    if let Some(grant) = &mut grant {
        grant.confirmed = reply.is_ok();
    }
    reply
}

pub(crate) fn allows(t: &Terminal, machine: &str, caller: Option<&str>) -> bool {
    let Some(id) = caller.and_then(parse_id).filter(|_| t.is_live()) else { return false };
    let grants = &t.reply_grants;
    grants.confirmed.get(machine).is_some_and(|ids| ids.contains(&id))
        || grants.pending.iter().any(|(m, recipient)| m == machine && *recipient == id)
}

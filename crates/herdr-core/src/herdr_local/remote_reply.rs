//! Replies to a local terminal that sent to a terminal on an added machine.

use crate::machines::Machine;
use crate::remote::{forward, is_peer};
use crate::rpc::RpcError;
use crate::runtime::Core;
use crate::terminals::{Terminal, address, parse_id};
use crate::wire::Conn;
use serde_json::{Value, json};
use std::sync::Arc;

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
        .filter(|id| {
            conn.is_none_or(|conn| !is_peer(core, conn))
                && core.lock().reg.terminals.get(id).is_some_and(Terminal::is_live)
        });
    let target = params["terminal"].as_str().filter(|target| !target.contains('/'));
    let Some((sender, target)) = sender.zip(target) else {
        return forward(core, machine, "terminal.send", params).await;
    };

    // Send to the resolved id so a reused name cannot grant access to the wrong terminal.
    let recipient = if let Some(id) = parse_id(target) {
        id
    } else {
        let found = forward(core, machine, "terminal.get", json!({"terminal": target})).await?;
        let id = found["address"]
            .as_str()
            .and_then(|address| address.split_once('/'))
            .filter(|(owner, _)| *owner == machine.name)
            .and_then(|(_, terminal)| parse_id(terminal))
            .ok_or_else(|| RpcError::internal(format!("{} did not identify {target:?}", machine.name)))?;
        params["terminal"] = address(id).into();
        id
    };

    let reply = forward(core, machine, "terminal.send", params).await?;
    let mut st = core.lock();
    if let Some(t) = st.reg.terminals.get_mut(&sender).filter(|t| t.is_live())
        && t.reply_grants.entry(machine.id.clone()).or_default().insert(recipient)
    {
        core.save(&st);
    }
    Ok(reply)
}

pub(crate) fn allows(t: &Terminal, machine: &str, caller: Option<&str>) -> bool {
    t.is_live()
        && caller.and_then(parse_id).is_some_and(|id| t.reply_grants.get(machine).is_some_and(|ids| ids.contains(&id)))
}

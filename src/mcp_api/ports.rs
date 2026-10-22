use super::*;
use crate::core::ports::RuleConfig;
fn set(hub: &Shared, client: &str, id: &str, rule_id: u64, enabled: bool) -> Result<Value> {
    let (rule, cursor) = {
        let h = hub.lock().unwrap();
        let s = h.ready(client, id)?;
        let (owner, rule) = s.ports.get(&rule_id).context("映射不存在")?;
        ensure!(owner == client, "映射属于另一个客户端");
        (rule.clone(), s.events.seq)
    };
    submit(
        hub,
        client,
        id,
        json!({"op":"port_mapping","id":rule_id,"enabled":enabled,"rule":rule}),
    )?;
    let result = wait_event(hub, client, id, cursor, Duration::from_secs(10), |v| {
        v["kind"] == "port_status" && v["data"]["id"] == rule_id && v["data"]["scope"] == "listener"
    })?;
    ensure!(
        result["data"]["status"] != "failed",
        "{}",
        result["data"]["message"]
    );
    if let Some((_, rule)) = hub
        .lock()
        .unwrap()
        .sessions
        .get_mut(id)
        .and_then(|s| s.ports.get_mut(&rule_id))
    {
        rule["enabled"] = json!(enabled);
        rule["status"] = result["data"]["status"].clone();
    }
    {
        let mut h = hub.lock().unwrap();
        let lease = format!("port:{rule_id}");
        if enabled {
            h.retain_resource(client, id, &lease, "port")?;
        } else {
            h.release_resource(id, &lease);
        }
    }
    Ok(result["data"].clone())
}
pub fn call(hub: &Shared, client: &str, name: &str, args: &Value) -> Result<Value> {
    let id = sid(args)?;
    // A restart must not drop the last connection owner between stop and start.
    let _lease = ResourceGuard::acquire(hub, client, id, "operation")?;
    let _guard = if name != "uu_port_list" {
        Some(Operation::acquire(hub, id, "ports")?)
    } else {
        None
    };
    if !matches!(name, "uu_port_create" | "uu_port_list") {
        let n = args["rule_id"].as_u64().context("缺少映射编号")?;
        let mut h = hub.lock().unwrap();
        let target = h.ready(client, id)?.target.clone();
        if !h.sessions[id].ports.contains_key(&n) {
            let saved = preferences::config(&h.directory);
            let mut rule = saved["port_rules"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|r| r["id"] == n && r["device_id"] == target)
                .cloned()
                .context("映射不存在")?;
            rule["enabled"] = json!(false);
            rule["persist"] = json!(true);
            h.sessions
                .get_mut(id)
                .unwrap()
                .ports
                .insert(n, (client.into(), rule));
        }
    }
    match name {
        "uu_port_list" => {
            let h = hub.lock().unwrap();
            let s = h.ready(client, id)?;
            let mut rules = s
                .ports
                .iter()
                .map(|(id, (owner, value))| json!({"id":id,"owned":owner==client,"rule":value}))
                .collect::<Vec<_>>();
            let saved = preferences::config(&h.directory);
            for rule in saved["port_rules"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|r| r["device_id"] == s.target)
            {
                if !s.ports.contains_key(&rule["id"].as_u64().unwrap_or(0)) {
                    let mut rule = rule.clone();
                    rule["enabled"] = json!(false);
                    rule["status"] = json!("disabled");
                    rule["persist"] = json!(true);
                    rules.push(json!({"id":rule["id"],"owned":true,"rule":rule}));
                }
            }
            Ok(json!({"rules":rules}))
        }
        "uu_port_create" => {
            let local = args["local_port"].as_u64().context("缺少本地端口")?;
            let target = args["target_port"].as_u64().context("缺少目标端口")?;
            ensure!(
                (1..=65535).contains(&local) && (1..=65535).contains(&target),
                "端口须在 1–65535 之间"
            );
            let (rule_id, rule) = {
                let mut h = hub.lock().unwrap();
                let s = h.ready(client, id)?;
                let rule_id =
                    ((uuid::Uuid::new_v4().as_u128() as u64) & 0x000f_ffff_ffff_ffff).max(1);
                let rule = RuleConfig {
                    id: rule_id,
                    device_id: s.target.clone(),
                    name: args["name"].as_str().unwrap_or("MCP 映射").into(),
                    local_port: local as u16,
                    target_host: args["target_host"].as_str().unwrap_or("127.0.0.1").into(),
                    target_port: target as u16,
                };
                rule.validate()?;
                let mut value = json!(rule);
                value["persist"] = json!(args["persist"].as_bool().unwrap_or(true));
                value["enabled"] = json!(false);
                h.sessions
                    .get_mut(id)
                    .unwrap()
                    .ports
                    .insert(rule_id, (client.into(), value.clone()));
                (rule_id, value)
            };
            let enabled = args["enabled"].as_bool().unwrap_or(true);
            let status = if enabled {
                match set(hub, client, id, rule_id, true) {
                    Ok(v) => v,
                    Err(e) => {
                        hub.lock()
                            .unwrap()
                            .sessions
                            .get_mut(id)
                            .unwrap()
                            .ports
                            .remove(&rule_id);
                        return Err(e);
                    }
                }
            } else {
                json!({"status":"disabled"})
            };
            if rule["persist"] == true {
                let h = hub.lock().unwrap();
                preferences::save_rule(&h.directory, &rule, false)?;
            }
            Ok(
                json!({"rule_id":rule_id,"local_address":format!("127.0.0.1:{local}"),"rule":rule,"status":status}),
            )
        }
        "uu_port_start" | "uu_port_stop" => set(
            hub,
            client,
            id,
            args["rule_id"].as_u64().context("缺少映射编号")?,
            name == "uu_port_start",
        ),
        "uu_port_delete" => {
            let n = args["rule_id"].as_u64().context("缺少映射编号")?;
            set(hub, client, id, n, false)?;
            let mut h = hub.lock().unwrap();
            let (_, rule) = h
                .sessions
                .get_mut(id)
                .unwrap()
                .ports
                .remove(&n)
                .context("映射不存在")?;
            preferences::save_rule(&h.directory, &rule, true)?;
            Ok(json!({"deleted":true}))
        }
        "uu_port_update" => {
            let n = args["rule_id"].as_u64().context("缺少映射编号")?;
            let (mut rule, enabled) = {
                let h = hub.lock().unwrap();
                let s = h.ready(client, id)?;
                let (owner, rule) = s.ports.get(&n).context("映射不存在")?;
                ensure!(owner == client, "映射属于其他客户端");
                (
                    rule.clone(),
                    args["enabled"].as_bool().unwrap_or(rule["enabled"] == true),
                )
            };
            for k in ["name", "local_port", "target_host", "target_port"] {
                if let Some(v) = args.get(k) {
                    rule[k] = v.clone();
                }
            }
            serde_json::from_value::<RuleConfig>(rule.clone())?.validate()?;
            set(hub, client, id, n, false)?;
            hub.lock()
                .unwrap()
                .sessions
                .get_mut(id)
                .unwrap()
                .ports
                .insert(n, (client.into(), rule.clone()));
            let result = set(hub, client, id, n, enabled)?;
            if rule["persist"] == true {
                let h = hub.lock().unwrap();
                preferences::save_rule(&h.directory, &rule, false)?;
            }
            Ok(result)
        }
        _ => bail!("未知映射操作"),
    }
}

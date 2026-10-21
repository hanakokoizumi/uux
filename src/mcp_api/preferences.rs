use super::*;
pub fn config(path: &std::path::Path) -> Value {
    crate::preferences_store::read(&path.join("mcp.json"))
}
pub fn save_rule(path: &std::path::Path, rule: &Value, remove: bool) -> Result<()> {
    let old = config(path);
    let mut updated = old.clone();
    let mut rules = old["port_rules"].as_array().cloned().unwrap_or_default();
    rules.retain(|r| r["id"] != rule["id"]);
    if !remove {
        let mut clean = rule.clone();
        for key in ["enabled", "status", "message"] {
            clean.as_object_mut().unwrap().remove(key);
        }
        rules.push(clean);
    }
    updated["port_rules"] = json!(rules);
    crate::preferences_store::merge(&path.join("mcp.json"), &old, &updated)?;
    Ok(())
}
pub fn call(hub: &Shared, client: &str, name: &str, args: &Value) -> Result<Value> {
    match name {
        "uu_mcp_status" => {
            let h = hub.lock().unwrap();
            Ok(
                json!({"enabled":h.enabled,"clients":h.clients.values().map(|c|json!({"kind":c.kind,"sessions":c.sessions.len()})).collect::<Vec<_>>(),"sessions":h.sessions.values().map(|s|s.status()).collect::<Vec<_>>(),"tasks":h.jobs.iter().map(|(id,j)|json!({"task_id":id,"session_id":j.session,"kind":j.kind,"state":j.state,"error":j.error})).collect::<Vec<_>>(),"configuration":{"mcpServers":{"uux":{"command":std::env::current_exe().ok(),"args":["--mcp","--state-dir",h.directory]}}}}),
            )
        }
        "uu_mcp_configure" => {
            let mut h = hub.lock().unwrap();
            ensure!(
                h.clients[client].kind == "gui",
                "MCP 开关只能从本机设置页修改"
            );
            let enabled = args["enabled"].as_bool().context("缺少 enabled")?;
            let old = config(&h.directory);
            let mut updated = old.clone();
            updated["enabled"] = json!(enabled);
            crate::preferences_store::merge(&h.directory.join("mcp.json"), &old, &updated)?;
            h.enabled = enabled;
            if !enabled {
                let clients = h
                    .clients
                    .iter()
                    .filter(|(_, c)| c.kind == "mcp")
                    .map(|(id, _)| id.clone())
                    .collect::<Vec<_>>();
                for id in clients {
                    h.remove_client(&id);
                }
            }
            Ok(json!({"enabled":enabled}))
        }
        "uu_favorites_list" => {
            let h = hub.lock().unwrap();
            let prefs = crate::service::preferences(&h.directory);
            Ok(
                json!({"favorites":prefs["favorite_devices"].as_array().into_iter().flatten().map(|v|json!({"id":v["id"],"name":v["name"],"has_saved_code":v["code"].as_str().is_some_and(|s|!s.is_empty())})).collect::<Vec<_>>()}),
            )
        }
        "uu_favorites_save" | "uu_favorites_delete" => {
            let id = crate::core::cloud::assistance_id(str_arg(args, "id")?)?;
            let mut h = hub.lock().unwrap();
            let path = h.directory.join("desktop.json");
            let old = crate::preferences_store::read(&path);
            let mut updated = old.clone();
            let mut favorites = old["favorite_devices"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let previous = favorites.iter().find(|v| v["id"] == id).cloned();
            favorites.retain(|v| v["id"] != id);
            if name == "uu_favorites_save" {
                let alias = args["name"]
                    .as_str()
                    .or_else(|| previous.as_ref().and_then(|v| v["name"].as_str()))
                    .unwrap_or("");
                let code = args["code"]
                    .as_str()
                    .or_else(|| previous.as_ref().and_then(|v| v["code"].as_str()))
                    .unwrap_or("");
                ensure!(
                    alias.chars().count() <= 40 && !alias.chars().any(char::is_control),
                    "备注名称最多 40 字"
                );
                ensure!(
                    code.len() <= 64 && !code.chars().any(char::is_control),
                    "验证码无效"
                );
                favorites.push(json!({"id":id,"name":alias,"code":code}));
            }
            updated["favorite_devices"] = json!(favorites);
            crate::preferences_store::merge(&path, &old, &updated)?;
            h.account_events
                .push(json!({"event":"native","kind":"preferences_changed","data":{}}));
            Ok(
                json!({"id":id,"saved":name=="uu_favorites_save","deleted":name=="uu_favorites_delete"}),
            )
        }
        "uu_device_rename" => {
            let id = str_arg(args, "device_id")?;
            let name = str_arg(args, "name")?;
            let request = uid();
            let cursor = {
                let h = hub.lock().unwrap();
                h.account.send(json!({"op":"device_action","request_id":request,"device_id":id,"action":"rename","name":name})).map_err(anyhow::Error::msg)?;
                h.account_events.seq
            };
            let start = Instant::now();
            loop {
                {
                    let h = hub.lock().unwrap();
                    for row in h.account_events.since(cursor) {
                        let v = &row["event"];
                        if v["kind"] == "device_action_result" && v["data"]["request_id"] == request
                        {
                            ensure!(v["data"]["ok"] == true, "{}", v["data"]["message"]);
                            let _ = h.account.send(json!({"op":"devices"}));
                            return Ok(json!({"renamed":true}));
                        }
                    }
                }
                ensure!(start.elapsed() < Duration::from_secs(20), "重命名超时");
                std::thread::sleep(Duration::from_millis(20));
            }
        }
        _ => bail!("未知设置操作"),
    }
}

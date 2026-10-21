use super::*;
use base64::Engine;
pub fn open(
    hub: &Shared,
    client: &str,
    id: &str,
    shell: &str,
    cols: u64,
    rows: u64,
) -> Result<String> {
    let handle = {
        let mut h = hub.lock().unwrap();
        h.ready(client, id)?;
        let s = h.sessions.get_mut(id).unwrap();
        ensure!(
            s.shell.as_ref().is_none_or(|s| s.closed),
            "这台设备的终端正在使用中"
        );
        let terminal = crate::service::Shell::new(client);
        let handle = terminal.id.clone();
        s.shell = Some(terminal);
        handle
    };
    if let Err(e) = submit(
        hub,
        client,
        id,
        json!({"op":"terminal","action":"open","shell":shell,"cols":cols,"rows":rows}),
    ) {
        let _ = close(hub, client, id, &handle);
        return Err(e);
    }
    let start = Instant::now();
    loop {
        {
            let h = hub.lock().unwrap();
            let s = h.ready(client, id)?;
            let t = s.shell.as_ref().context("终端不存在")?;
            if let Some(e) = &t.error {
                let error = e.clone();
                drop(h);
                let _ = close(hub, client, id, &handle);
                bail!("{error}");
            }
            ensure!(!t.closed, "终端启动时退出");
            if t.ready && !t.bytes.is_empty() {
                return Ok(handle);
            }
        }
        if start.elapsed() > Duration::from_secs(15) {
            let _ = close(hub, client, id, &handle);
            bail!("终端启动超时");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}
fn owns(hub: &Shared, client: &str, id: &str, handle: &str) -> Result<()> {
    let h = hub.lock().unwrap();
    let t = h.ready(client, id)?.shell.as_ref().context("终端不存在")?;
    ensure!(
        t.owner == client && t.id == handle,
        "终端不属于本客户端或已被替换"
    );
    Ok(())
}
pub fn close(hub: &Shared, client: &str, id: &str, handle: &str) -> Result<Value> {
    owns(hub, client, id, handle)?;
    if hub
        .lock()
        .unwrap()
        .ready(client, id)?
        .shell
        .as_ref()
        .is_some_and(|t| t.closed)
    {
        return Ok(json!({"terminal_id":handle,"closed":true}));
    }
    submit(hub, client, id, json!({"op":"terminal","action":"close"}))?;
    let start = Instant::now();
    let mut verify = Instant::now();
    loop {
        {
            let h = hub.lock().unwrap();
            let s = h.owner(client, id)?;
            if s.shell.as_ref().is_some_and(|t| t.id == handle && t.closed) {
                return Ok(json!({"terminal_id":handle,"closed":true}));
            }
        }
        if verify.elapsed() > Duration::from_millis(350) {
            let _ = session_send(
                hub,
                client,
                id,
                json!({"op":"terminal","action":"verify_closed"}),
            );
            verify = Instant::now();
        }
        ensure!(
            start.elapsed() < Duration::from_secs(5),
            "终端关闭已发送，未能确认远端退出"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}
fn input(hub: &Shared, client: &str, id: &str, handle: &str, text: &str) -> Result<Value> {
    owns(hub, client, id, handle)?;
    ensure!(text.len() <= 1024 * 1024, "终端输入过长");
    submit(
        hub,
        client,
        id,
        json!({"op":"terminal","action":"input","data":base64::engine::general_purpose::STANDARD.encode(text)}),
    )
}
fn encoded(text: &str) -> String {
    let bytes = text
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect::<Vec<_>>();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}
pub fn wrap(command: &str, cwd: Option<&str>, marker: &str) -> String {
    let cd = cwd
        .map(|s| {
            format!(
                "Set-Location -LiteralPath '{}' -ErrorAction Stop;",
                s.replace('\'', "''")
            )
        })
        .unwrap_or_default();
    let script = format!(
        "$ProgressPreference='SilentlyContinue'; [Console]::OutputEncoding=[Text.UTF8Encoding]::new(); $OutputEncoding=[Console]::OutputEncoding; $ErrorActionPreference='Continue'; try {{ {cd} & {{ {command}\n }}; $uu_ok=$?; $uu_exit=$LASTEXITCODE; if (!$uu_ok -and !$uu_exit) {{exit 1}}; if ($uu_exit) {{exit $uu_exit}}; exit 0 }} catch {{[Console]::Error.WriteLine($_.ToString());exit 1}}"
    );
    let output = base64::engine::general_purpose::STANDARD.encode(format!("UU_OUT_{marker}:"));
    let done = base64::engine::general_purpose::STANDARD.encode(format!("UU_DONE_{marker}:"));
    let wrapper = include_str!("command.ps1")
        .replace("__OUTPUT_PREFIX__", &output)
        .replace("__DONE_PREFIX__", &done)
        .replace("__SCRIPT__", &encoded(&script));
    wrapper.lines().map(str::trim).collect::<Vec<_>>().join(" ") + "\r"
}
fn console_text(bytes: &[u8], rows: u16, cols: u16) -> String {
    let mut parser = vt100::Parser::new(rows, cols, 2048);
    parser.process(bytes);
    parser.screen_mut().set_scrollback(usize::MAX);
    let history = parser.screen().scrollback();
    let mut text = String::new();
    for offset in (1..=history).rev() {
        parser.screen_mut().set_scrollback(offset);
        if let Some(row) = parser.screen().rows(0, cols).next() {
            text.push_str(&row);
            text.push('\n');
        }
    }
    parser.screen_mut().set_scrollback(0);
    text.push_str(&parser.screen().contents());
    text
}
fn flattened(bytes: &[u8]) -> String {
    console_text(bytes, 40, 200).replace('\n', "")
}

fn marker_result(bytes: &[u8], marker: &str) -> Option<(usize, i64)> {
    let prefix = format!("UU_DONE_{marker}:");
    for (at, _) in bytes
        .windows(prefix.len())
        .enumerate()
        .filter(|(_, w)| *w == prefix.as_bytes())
    {
        let rest = &bytes[at + prefix.len()..];
        let Some(end) = rest.windows(7).position(|b| b == b":UU_END") else {
            continue;
        };
        if let Ok(text) = std::str::from_utf8(&rest[..end])
            && let Ok(code) = text.trim().parse()
        {
            return Some((at, code));
        }
    }
    None
}
fn command_output(bytes: &[u8], marker: &str) -> String {
    let prefix = format!("UU_OUT_{marker}:");
    let mut output = String::new();
    {
        let text = String::from_utf8_lossy(bytes);
        for record in text.split(&prefix).skip(1) {
            if let Some((payload, _)) = record.split_once(":UU_OUT_END") {
                let payload = payload
                    .chars()
                    .filter(|c| !c.is_whitespace())
                    .collect::<String>();
                if let Ok(data) = base64::engine::general_purpose::STANDARD.decode(payload) {
                    output.push_str(&String::from_utf8_lossy(&data));
                }
            }
        }
    }
    output
}
pub fn clean_output(bytes: &[u8]) -> String {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 27 {
            i += 1;
            if i < bytes.len() && bytes[i] == b'[' {
                i += 1;
                while i < bytes.len() {
                    let b = bytes[i];
                    i += 1;
                    if (0x40..=0x7e).contains(&b) {
                        break;
                    }
                }
            } else if i < bytes.len() && bytes[i] == b']' {
                i += 1;
                while i < bytes.len() {
                    if bytes[i] == 7 {
                        i += 1;
                        break;
                    }
                    if bytes[i] == 27 && bytes.get(i + 1) == Some(&b'\\') {
                        i += 2;
                        break;
                    }
                    i += 1;
                }
            } else {
                i += 1;
            }
        } else {
            if bytes[i] != b'\r' {
                out.push(bytes[i]);
            }
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
pub fn call(hub: &Shared, client: &str, name: &str, args: &Value) -> Result<Value> {
    let id = sid(args)?;
    if name == "uu_command_exec" {
        let command = str_arg(args, "command")?.to_owned();
        ensure!(
            command.len() <= 8192,
            "命令最多 8 KiB；较长脚本请先上传文件再执行"
        );
        let cwd = args["cwd"].as_str().map(str::to_owned);
        let timeout = args["timeout_ms"]
            .as_u64()
            .unwrap_or(60000)
            .clamp(1000, 3600000);
        return start_job(hub, client, id, "command", move |h, c, s, j| {
            let marker = uid();
            let script = wrap(&command, cwd.as_deref(), &marker);
            ensure!(
                script.encode_utf16().count() < 30000,
                "命令与工作路径过长，请上传脚本文件后执行"
            );
            let terminal = open(&h, &c, &s, "powershell", 200, 40)?;
            let start = Instant::now();
            let result = (|| {
                let init = uid();
                let encoded_init = base64::engine::general_purpose::STANDARD.encode(&init);
                input(
                    &h,
                    &c,
                    &s,
                    &terminal,
                    &format!(
                        "Set-PSReadLineOption -HistorySaveStyle SaveNothing; Remove-Module PSReadLine; $global:uu_prompt=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{encoded_init}'))+'>'; function global:prompt {{ $global:uu_prompt+' ' }}\r"
                    ),
                )?;
                let init_started = Instant::now();
                let mut quiet = Instant::now();
                let mut observed = 0;
                loop {
                    check_cancel(&h, &j)?;
                    let ready = {
                        let h = h.lock().unwrap();
                        let t = h.ready(&c, &s)?.shell.as_ref().context("终端消失")?;
                        if t.bytes.len() != observed {
                            observed = t.bytes.len();
                            quiet = Instant::now();
                        }
                        flattened(&t.bytes)
                            .trim_end()
                            .ends_with(&format!("{init}>"))
                            && quiet.elapsed() >= Duration::from_millis(300)
                    };
                    if ready {
                        break;
                    }
                    ensure!(
                        init_started.elapsed() < Duration::from_secs(15),
                        "命令终端初始化未确认"
                    );
                    std::thread::sleep(Duration::from_millis(30));
                }
                input(&h, &c, &s, &terminal, &script)?;
                loop {
                    check_cancel(&h, &j)?;
                    ensure!(
                        start.elapsed() < Duration::from_millis(timeout),
                        "命令执行超时"
                    );
                    let (bytes, offset, closed, error) = {
                        let h = h.lock().unwrap();
                        let t = h.ready(&c, &s)?.shell.as_ref().context("终端消失")?;
                        (t.bytes.clone(), t.offset, t.closed, t.error.clone())
                    };
                    let plain = flattened(&bytes);
                    let done = marker_result(plain.as_bytes(), &marker);
                    let output = command_output(plain.as_bytes(), &marker);
                    let value = json!({"output":output,"output_stream":"merged","truncated":offset>0,"exit_code":done.map(|(_,code)|code),"elapsed_ms":start.elapsed().as_millis() as u64});
                    job_update(&h, &j, value.clone());
                    if done.is_some() {
                        return Ok(value);
                    }
                    if let Some(error) = error {
                        bail!("{error}");
                    }
                    ensure!(!closed, "终端提前退出，未取得命令完成结果");
                    std::thread::sleep(Duration::from_millis(40));
                }
            })();
            let cleanup = close(&h, &c, &s, &terminal);
            match (result, cleanup) {
                (Ok(mut value), Ok(_)) => {
                    value["terminal_closed"] = json!(true);
                    Ok(value)
                }
                (Err(error), _) => Err(error),
                (Ok(_), Err(error)) => Err(error),
            }
        });
    }
    if name == "uu_terminal_open" {
        let handle = open(
            hub,
            client,
            id,
            args["shell"].as_str().unwrap_or("powershell"),
            args["cols"].as_u64().unwrap_or(100),
            args["rows"].as_u64().unwrap_or(30),
        )?;
        return Ok(json!({"terminal_id":handle,"ready":true,"cursor":0}));
    }
    let handle = str_arg(args, "terminal_id")?;
    owns(hub, client, id, handle)?;
    match name {
        "uu_terminal_close" => close(hub, client, id, handle),
        "uu_terminal_write" => input(
            hub,
            client,
            id,
            handle,
            args["text"].as_str().context("缺少 text")?,
        ),
        "uu_terminal_resize" => submit(
            hub,
            client,
            id,
            json!({"op":"terminal","action":"resize","cols":args["cols"],"rows":args["rows"]}),
        ),
        "uu_terminal_read" => {
            let cursor = args["cursor"].as_u64().unwrap_or(0);
            let wait = args["wait_ms"].as_u64().unwrap_or(0).min(10000);
            let start = Instant::now();
            loop {
                {
                    let h = hub.lock().unwrap();
                    let t = h.ready(client, id)?.shell.as_ref().context("终端不存在")?;
                    let end = t.offset + t.bytes.len() as u64;
                    ensure!(cursor <= end, "输出游标超出范围");
                    if end > cursor || t.closed || start.elapsed() >= Duration::from_millis(wait) {
                        let from = cursor.saturating_sub(t.offset) as usize;
                        let to = (from + 64 * 1024).min(t.bytes.len());
                        let bytes = &t.bytes[from..to];
                        return Ok(
                            json!({"terminal_id":handle,"text":clean_output(bytes),"data_base64":base64::engine::general_purpose::STANDARD.encode(bytes),"cursor":t.offset+to as u64,"gap":cursor<t.offset,"closed":t.closed,"exit_code":t.exit_code,"error":t.error}),
                        );
                    }
                }
                std::thread::sleep(Duration::from_millis(25));
            }
        }
        _ => bail!("未知终端工具"),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn completion_marker_cannot_match_command_echo() {
        let marker = "abc-123";
        let script = wrap("Write-Output hello", Some("C:\\a'b"), marker);
        assert!(marker_result(script.as_bytes(), marker).is_none());
        assert_eq!(
            marker_result(b"helloUU_DONE_abc-123:7:UU_END", marker),
            Some((5, 7))
        );
        assert!(marker_result(b"helloUU_DONE_abc-123:7", marker).is_none());
    }
    #[test]
    fn console_records_preserve_unicode_and_ignore_echo() {
        let marker = "test-uuid";
        let data = base64::engine::general_purpose::STANDARD.encode("第一行\r\n第二行\r\n");
        let stream = format!(
            "PS> echo UU_DONE_{marker}:%errorlevel%:UU_END\r\nUU_OUT_{marker}:{}\x1b[0m{}:UU_OUT_ENDUU_DONE_{marker}:7:UU_END",
            &data[..4],
            &data[4..]
        );
        let plain = flattened(stream.as_bytes());
        assert_eq!(
            command_output(plain.as_bytes(), marker),
            "第一行\r\n第二行\r\n"
        );
        assert_eq!(marker_result(plain.as_bytes(), marker).unwrap().1, 7);
    }
    #[test]
    fn ansi_is_removed_without_losing_unicode() {
        assert_eq!(clean_output("\x1b[31m你好\x1b[0m\r\n".as_bytes()), "你好\n");
    }
}

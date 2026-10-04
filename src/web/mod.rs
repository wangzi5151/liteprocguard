//! 内置轻量 Web UI（无任何外部前端依赖，仅监听 127.0.0.1）。
//!
//! * 随机令牌鉴权，防止本机其它网页发起 CSRF 请求；
//! * 手写极简 HTTP/1.1 处理，只支持本项目需要的 GET / POST；
//! * 静态资源内嵌进二进制，无需额外文件。

use crate::cli::command;
use crate::core::engine;
use crate::core::model::{Priority, RuleSet};
use crate::platform::{process, temperature};
use crate::utils::{config, logger};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::Ordering;
use std::time::Duration;

const INDEX_HTML: &str = include_str!("index.html");

/// 启动 Web UI（阻塞），直到收到 Ctrl+C / SHUTDOWN。
pub fn serve(port: u16) -> Result<(), String> {
    let token = crate::utils::random_token();
    let addr = format!("127.0.0.1:{}", port);
    let listener = TcpListener::bind(&addr).map_err(|e| format!("绑定 {} 失败：{}", addr, e))?;
    listener
        .set_nonblocking(true)
        .map_err(|e| format!("设置非阻塞失败：{}", e))?;

    println!("LiteProcGuard Web UI 已启动：");
    println!("  http://127.0.0.1:{}/?token={}", port, token);
    println!("  仅监听本机回环地址，令牌用于防止跨站请求；按 Ctrl+C 退出。");

    loop {
        if engine::SHUTDOWN.load(Ordering::Relaxed) {
            break;
        }
        match listener.accept() {
            Ok((stream, _)) => {
                let token = token.clone();
                std::thread::spawn(move || {
                    let _ = handle_connection(stream, &token);
                });
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(200));
            }
            Err(e) => {
                eprintln!("接受连接失败：{}", e);
                break;
            }
        }
    }
    Ok(())
}

/// 在后台线程启动 Web UI。
pub fn serve_background(port: u16) {
    std::thread::spawn(move || {
        if let Err(e) = serve(port) {
            eprintln!("{}", e);
        }
    });
}

fn handle_connection(mut stream: TcpStream, token: &str) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(5)))?;
    let mut buf: Vec<u8> = Vec::new();
    let mut tmp = [0u8; 8192];
    let headers_end;
    loop {
        let n = match stream.read(&mut tmp) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(_) => return Ok(()),
        };
        buf.extend_from_slice(&tmp[..n]);
        if let Some(pos) = find_headers_end(&buf) {
            headers_end = pos;
            break;
        }
        if buf.len() > 128 * 1024 {
            return Ok(());
        }
    }

    let head = String::from_utf8_lossy(&buf[..headers_end.0]).to_string();
    let mut lines = head.split("\r\n");
    let request_line = lines.next().unwrap_or("");
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("GET").to_string();
    let target = parts.next().unwrap_or("/").to_string();

    let content_length = lines
        .filter_map(|l| l.split_once(':'))
        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
        .and_then(|(_, v)| v.trim().parse::<usize>().ok())
        .unwrap_or(0);

    let mut body = buf[headers_end.1..].to_vec();
    while body.len() < content_length {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&tmp[..n]),
            Err(_) => break,
        }
    }
    if body.len() > content_length {
        body.truncate(content_length);
    }

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), q.to_string()),
        None => (target.clone(), String::new()),
    };

    // 首页无需令牌，浏览器打开后由前端从 URL 读取令牌。
    if path == "/" || path == "/index.html" {
        return respond(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            INDEX_HTML.as_bytes(),
        );
    }

    if !token_ok(&query, token) {
        return respond(
            &mut stream,
            403,
            "application/json",
            r#"{"error":"无效或缺失令牌"}"#.as_bytes(),
        );
    }

    let result = route(&method, &path, &query, &body);
    match result {
        Ok((ct, data)) => respond(&mut stream, 200, ct, &data),
        Err(e) => respond(&mut stream, 500, "application/json", e.as_bytes()),
    }
}

fn find_headers_end(buf: &[u8]) -> Option<(usize, usize)> {
    buf.windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| (p, p + 4))
}

fn token_ok(query: &str, token: &str) -> bool {
    query.split('&').any(|kv| {
        kv.split_once('=')
            .map(|(k, v)| k == "token" && v == token)
            .unwrap_or(false)
    })
}

type RouteResult = Result<(&'static str, Vec<u8>), String>;

fn json_err(msg: &str) -> String {
    serde_json::json!({ "error": msg }).to_string()
}

fn route(method: &str, path: &str, query: &str, body: &[u8]) -> RouteResult {
    match (method, path) {
        ("GET", "/api/status") => {
            let backend = crate::platform::limiter::CpuLimiter::detect_backend();
            let value = serde_json::json!({
                "version": env!("CARGO_PKG_VERSION"),
                "backend": backend,
                "guard_running": command::guard_running(),
                "temperature": temperature::read_celsius(),
                "temperature_available": temperature::available(),
                "elevated": process::is_elevated(),
                "data_dir": crate::utils::base_dir().to_string_lossy(),
            });
            Ok(("application/json", value.to_string().into_bytes()))
        }
        ("GET", "/api/processes") => {
            let mut procs = process::list_processes();
            procs.truncate(300);
            let data = serde_json::to_vec(&procs).map_err(|e| json_err(&e.to_string()))?;
            Ok(("application/json", data))
        }
        ("GET", "/api/rulesets") => {
            let value = serde_json::json!(config::list_rulesets());
            Ok(("application/json", value.to_string().into_bytes()))
        }
        ("GET", "/api/ruleset") => {
            let name = query_param(query, "name").unwrap_or_default();
            let rs = if name.is_empty() {
                command::resolve_ruleset(None).map_err(|e| json_err(&e))?
            } else {
                config::load_ruleset(&name).map_err(|e| json_err(&e))?
            };
            let data = serde_json::to_vec(&rs).map_err(|e| json_err(&e.to_string()))?;
            Ok(("application/json", data))
        }
        ("POST", "/api/ruleset") => {
            let rs: RuleSet = serde_json::from_slice(body)
                .map_err(|e| json_err(&format!("规则 JSON 解析失败：{}", e)))?;
            command::persist_ruleset(&rs).map_err(|e| json_err(&e))?;
            Ok(("application/json", br#"{"ok":true}"#.to_vec()))
        }
        ("DELETE", "/api/ruleset") => {
            let name = query_param(query, "name").unwrap_or_default();
            config::delete_ruleset(&name).map_err(|e| json_err(&e))?;
            Ok(("application/json", br#"{"ok":true}"#.to_vec()))
        }
        ("POST", "/api/guard/start") => {
            #[derive(serde::Deserialize)]
            struct Req {
                #[serde(default = "def_interval")]
                interval: u64,
                #[serde(default)]
                ruleset: String,
                #[serde(default)]
                temperature: bool,
            }
            fn def_interval() -> u64 {
                2
            }
            let req: Req = serde_json::from_slice(body).unwrap_or(Req {
                interval: 2,
                ruleset: String::new(),
                temperature: false,
            });
            let rs_name = if req.ruleset.is_empty() {
                None
            } else {
                Some(req.ruleset.clone())
            };
            let ruleset = command::resolve_ruleset(rs_name).map_err(|e| json_err(&e))?;
            let _ = command::guard_stop();
            command::guard_background(
                ruleset,
                engine::EngineConfig {
                    interval_secs: req.interval.max(1),
                    temperature_enabled: req.temperature,
                    dry_run: false,
                },
            )
            .map_err(|e| json_err(&e))?;
            Ok(("application/json", br#"{"ok":true}"#.to_vec()))
        }
        ("POST", "/api/guard/stop") => {
            command::guard_stop();
            Ok(("application/json", br#"{"ok":true}"#.to_vec()))
        }
        ("POST", "/api/clear") => {
            engine::emergency_clear();
            Ok(("application/json", br#"{"ok":true}"#.to_vec()))
        }
        ("GET", "/api/logs") => {
            let tail = query_param(query, "tail")
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(100);
            let lines = logger::tail(tail);
            Ok(("text/plain; charset=utf-8", lines.join("\n").into_bytes()))
        }
        ("POST", "/api/logs/clear") => {
            logger::clear();
            Ok(("application/json", br#"{"ok":true}"#.to_vec()))
        }
        ("POST", "/api/priority") => {
            #[derive(serde::Deserialize)]
            struct Req {
                pid: u32,
                priority: String,
            }
            let req: Req =
                serde_json::from_slice(body).map_err(|e| json_err(&format!("参数错误：{}", e)))?;
            let priority = match req.priority.to_ascii_lowercase().as_str() {
                "high" => Priority::High,
                "above" => Priority::AboveNormal,
                "normal" => Priority::Normal,
                "below" => Priority::BelowNormal,
                "idle" => Priority::Idle,
                other => return Err(json_err(&format!("未知优先级：{}", other))),
            };
            process::set_priority(req.pid, priority).map_err(|e| json_err(&e))?;
            Ok(("application/json", br#"{"ok":true}"#.to_vec()))
        }
        _ => Err(json_err("未知接口")),
    }
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        kv.split_once('=')
            .filter(|(k, _)| *k == key)
            .map(|(_, v)| v.to_string())
    })
}

fn respond(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
) -> std::io::Result<()> {
    let reason = match status {
        200 => "OK",
        403 => "Forbidden",
        404 => "Not Found",
        _ => "Internal Server Error",
    };
    let header = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        status,
        reason,
        content_type,
        body.len()
    );
    stream.write_all(header.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}

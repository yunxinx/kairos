//! 配置驱动的端到端测试：配置文件 → 建库 → 监听 → 未实现路径的确定响应。

mod common;

use std::{io::Write, net::TcpListener as StdListener};

use kairos::{config, gateway, store};

/// 用配置文件启动网关，验证监听配置生效、未实现路径返回 404。
#[tokio::test]
async fn config_driven_listen_and_fallback() {
    // 先拿到一个空闲端口，写进配置，再让网关监听该端口。
    let free = StdListener::bind("127.0.0.1:0").expect("应能绑定空闲端口");
    let port = free.local_addr().expect("应能取端口").port();
    drop(free);

    let dir = tempfile::tempdir().expect("应能创建临时目录");
    let config_path = dir.path().join("config.json");
    let mut f = std::fs::File::create(&config_path).expect("应能写配置文件");
    write!(
        f,
        r#"{{
            "listen": {{ "host": "127.0.0.1", "port": {port} }},
            "database": {{ "path": "./kairos.db" }},
            "channels": [{{
                "name": "c", "protocol": "openai_chat",
                "base_url": "http://127.0.0.1:1", "api_key": "k",
                "priority": 1, "weight": 1, "timeout_ms": 1000, "max_retries": 0
            }}],
            "prices": {{ "gpt-4o": {{ "input": 1.0, "output": 2.0 }} }}
        }}"#
    )
    .expect("应能写入配置");
    drop(f);

    let cfg = config::Config::load(&config_path).expect("配置应可解析");
    let pool = store::open(&cfg.database.path).await.expect("建库应成功");
    let app = gateway::router(&cfg, pool);

    let listen = format!("{}:{}", cfg.listen.host, cfg.listen.port);
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .expect("应能监听配置端口");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("网关服务应运行");
    });

    // 配置的端口上，未实现路径返回确定 404。
    let client = reqwest::Client::new();
    let resp = client
        .get(format!("http://127.0.0.1:{port}/v1/responses"))
        .send()
        .await
        .expect("应能请求网关未实现路径");
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::NOT_FOUND,
        "未实现路径应 404"
    );
    let body = resp.text().await.expect("响应应可读");
    assert!(body.contains("未实现"), "响应应含可读提示，实际 {body:?}");

    // 数据库按配置路径建出（相对路径已相对配置文件目录解析）。
    let db_exists = dir.path().join("kairos.db").exists();
    assert!(db_exists, "数据库应建在配置目录内");
}

/// 配置示例文件本身应能被解析通过（防样例与 schema 漂移）。
#[test]
fn example_config_file_is_valid() {
    let path = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/config.example.json"));
    let cfg = config::Config::load(path).expect("示例配置应可解析");
    assert_eq!(cfg.listen.port, 8787);
    assert_eq!(cfg.channels.len(), 1);
    assert_eq!(cfg.prices.0.len(), 1);
    assert_eq!(
        cfg.database.path,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("kairos.db"),
        "示例配置的相对路径应相对示例文件目录解析"
    );
}

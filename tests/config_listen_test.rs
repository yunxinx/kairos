//! 配置驱动的端到端测试：配置文件 → 建库 → 加载快照 → 监听 → 未实现路径的确定响应。

mod common;

use std::{io::Write, net::TcpListener as StdListener};

use kairos::{config, gateway, runtime, store};

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
            "admin_key": "sk-admin"
        }}"#
    )
    .expect("应能写入配置");
    drop(f);

    let cfg = config::Config::load(&config_path).expect("配置应可解析");
    let pool = store::open(&cfg.database.path).await.expect("建库应成功");
    // 空库加载出快照（无资源），网关可正常启动；本测试只验证监听与未实现路径。
    let snapshot = runtime::load_snapshot(&pool).await.expect("应能加载快照");
    let snapshot = runtime::snapshot_handle(snapshot);
    let app = gateway::router(pool, snapshot).await;

    let listen = format!("{}:{}", cfg.listen.host, cfg.listen.port);
    let listener = tokio::net::TcpListener::bind(&listen)
        .await
        .expect("应能监听配置端口");
    tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .expect("网关服务应运行");
    });

    // 配置的端口上，未实现路径返回确定 404。
    // embeddings / image generation / audio / batch 均属 spec Out of Scope。
    let client = reqwest::Client::new();
    for path in [
        "/v1/embeddings",
        "/v1/images/generations",
        "/v1/audio/speech",
        "/v1/batches",
        "/v1/realtime",
        "/metrics",
    ] {
        for method in [reqwest::Method::GET, reqwest::Method::POST] {
            let resp = client
                .request(method.clone(), format!("http://127.0.0.1:{port}{path}"))
                .send()
                .await
                .expect("应能请求网关未实现路径");
            assert_eq!(
                resp.status(),
                reqwest::StatusCode::NOT_FOUND,
                "Out of Scope 路径 {method} {path} 应 404"
            );
            let body = resp.text().await.expect("响应应可读");
            assert!(body.contains("未实现"), "响应应含可读提示，实际 {body:?}");
        }
    }

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
    assert_eq!(cfg.admin_key, "sk-admin-secret");
    let admin = cfg.admin_listen.expect("示例配置应含管理监听");
    assert_eq!(admin.port, 8788);
    assert_eq!(
        cfg.database.path,
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("kairos.db"),
        "示例配置的相对路径应相对示例文件目录解析"
    );
}

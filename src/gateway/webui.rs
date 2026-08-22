//! 管理面静态资源：编译期嵌入 `webui/dist`，缺失时退化为纯 API。
//!
//! 认证边界由调用方保证：本模块只作为管理路由的 fallback，不经过 admin key
//! 中间件。已注册的资源 API 仍由 [`super::admin`] 的中间件保护。

use axum::{
    http::{
        HeaderValue, Method, StatusCode, Uri,
        header::{self, CACHE_CONTROL, CONTENT_TYPE, X_CONTENT_TYPE_OPTIONS},
    },
    response::{IntoResponse, Response},
};
use rust_embed::Embed;

/// 嵌入的 Web UI 构建产物。`webui/dist` 缺失时允许编译（空嵌入）。
///
/// debug 构建按文件系统读取该目录（改 UI 产物不必重编）；release 把文件打进二进制。
#[derive(Embed)]
#[folder = "webui/dist"]
#[allow_missing = true]
struct Assets;

/// 是否嵌入了可服务的 `index.html`（产物存在且构建完整）。
pub fn is_available() -> bool {
    Assets::get("index.html").is_some()
}

/// SPA + 静态文件：GET/HEAD 命中文件则返回；否则在 UI 可用时回退 `index.html`。
///
/// 带扩展名却找不到的路径视为缺失静态资源（404），避免把 `/foo.js` 当成前端路由。
pub async fn serve(uri: Uri, method: Method) -> Response {
    if method != Method::GET && method != Method::HEAD {
        return not_found();
    }
    let path = uri.path().trim_start_matches('/');
    if path.contains("..") {
        return not_found();
    }
    if path.is_empty() || path == "index.html" {
        return index_html();
    }
    match Assets::get(path) {
        Some(file) => file_response(path, file.data),
        None if path.contains('.') => not_found(),
        None => index_html(),
    }
}

/// 回退首页；产物缺失时 404（管理面退化为纯 API，不视为启动错误）。
fn index_html() -> Response {
    match Assets::get("index.html") {
        Some(file) => file_response("index.html", file.data),
        None => not_found(),
    }
}

fn file_response(path: &str, data: std::borrow::Cow<'static, [u8]>) -> Response {
    (
        [
            (
                CONTENT_TYPE,
                HeaderValue::from_static(content_type_for(path)),
            ),
            (
                CACHE_CONTROL,
                HeaderValue::from_static(cache_control_for(path)),
            ),
            (X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff")),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static(CSP),
            ),
        ],
        data,
    )
        .into_response()
}

/// 管理台内容安全策略：全部资源仅同源 + 头像 data URL。
///
/// `style-src` 放开 `unsafe-inline` 是 Vue `:style` 绑定（内联 style 属性）所需，
/// 脚本无此豁免——主题引导已外置为 `/bootstrap.js`，Vite 产物全部同源打包。
const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
                   img-src 'self' data:; font-src 'self'; connect-src 'self'; object-src 'none'; \
                   base-uri 'self'; frame-ancestors 'none'";

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "路径未实现").into_response()
}

/// html 与未指纹化资源不缓存；Vite 带内容哈希的资产可长期 immutable。
fn cache_control_for(path: &str) -> &'static str {
    if path == "index.html" || path.ends_with(".html") {
        "no-cache"
    } else if is_fingerprinted_asset(path) {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    }
}

/// Vite 默认文件名：`index-Ab12Cd34.js`（stem 以 `-` + ≥8 位字母数字哈希结尾）。
fn is_fingerprinted_asset(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let Some((stem, _)) = name.rsplit_once('.') else {
        return false;
    };
    stem.rsplit_once('-')
        .is_some_and(|(_, hash)| hash.len() >= 8 && hash.chars().all(|c| c.is_ascii_alphanumeric()))
}

/// 按扩展名给出 Content-Type；未知类型按 octet-stream，避免引入额外 crate。
fn content_type_for(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html") => "text/html; charset=utf-8",
        Some("js") | Some("mjs") => "text/javascript",
        Some("css") => "text/css",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("ico") => "image/x-icon",
        Some("webp") => "image/webp",
        Some("woff") => "font/woff",
        Some("woff2") => "font/woff2",
        Some("json") | Some("map") => "application/json",
        Some("txt") => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::serve;
    use axum::http::{Method, StatusCode, header};

    /// 所有静态响应都带 CSP 与 nosniff；脚本只允许同源。
    /// `webui/dist` 缺失（纯 API 构建）时无静态响应可测，直接跳过。
    #[tokio::test]
    async fn static_responses_carry_csp() {
        if !super::is_available() {
            return;
        }
        let response = serve("/".parse().expect("URI 应能构造"), Method::GET).await;
        assert_eq!(response.status(), StatusCode::OK);
        let csp = response
            .headers()
            .get(header::CONTENT_SECURITY_POLICY)
            .and_then(|value| value.to_str().ok())
            .expect("应有 CSP 头");
        assert!(csp.contains("script-src 'self'"), "{csp}");
        assert!(!csp.contains("script-src 'unsafe-inline'"), "{csp}");
        assert!(
            response
                .headers()
                .get(header::X_CONTENT_TYPE_OPTIONS)
                .is_some()
        );
    }
}

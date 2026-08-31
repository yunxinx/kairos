//! 出站网络客户端与目标地址策略。

use std::net::{IpAddr, SocketAddr};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use thiserror::Error;

/// 按请求快照选择的两类出站客户端。
#[derive(Clone)]
pub(super) struct OutboundClients {
    unrestricted: reqwest::Client,
    public_only: reqwest::Client,
}

impl OutboundClients {
    pub(super) fn new() -> Self {
        let unrestricted = base_client_builder()
            .build()
            .expect("未配置会失败的 reqwest 客户端选项");
        let public_only = base_client_builder()
            .no_proxy()
            .pool_max_idle_per_host(0)
            .dns_resolver(PublicNetworkResolver)
            .build()
            .expect("未配置会失败的 reqwest 客户端选项");
        Self {
            unrestricted,
            public_only,
        }
    }

    pub(super) fn for_policy(&self, allow_private_networks: bool) -> &reqwest::Client {
        if allow_private_networks {
            &self.unrestricted
        } else {
            &self.public_only
        }
    }
}

fn base_client_builder() -> reqwest::ClientBuilder {
    reqwest::Client::builder().redirect(reqwest::redirect::Policy::none())
}

#[derive(Debug, Error)]
pub(super) enum TargetError {
    #[error("上游地址无效")]
    InvalidUrl,
    #[error("上游地址缺少主机名")]
    MissingHost,
    #[error("上游地址被安全策略拒绝")]
    Restricted,
}

/// IP 字面量不会经过 DNS 解析器，需在创建请求前执行同一策略。
pub(super) fn validate_target(raw: &str, allow_private_networks: bool) -> Result<(), TargetError> {
    let parsed = reqwest::Url::parse(raw).map_err(|_| TargetError::InvalidUrl)?;
    let host = parsed.host_str().ok_or(TargetError::MissingHost)?;
    if !allow_private_networks
        && let Ok(address) = host
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .unwrap_or(host)
            .parse::<IpAddr>()
        && is_restricted_address(address)
    {
        return Err(TargetError::Restricted);
    }
    Ok(())
}

#[derive(Clone)]
struct PublicNetworkResolver;

impl Resolve for PublicNetworkResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        Box::pin(async move {
            let addresses = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|err| Box::new(err) as Box<dyn std::error::Error + Send + Sync>)?
                .collect::<Vec<SocketAddr>>();
            if addresses.is_empty() {
                return Err(Box::new(std::io::Error::other("上游主机没有可用地址"))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            if addresses
                .iter()
                .any(|address| is_restricted_address(address.ip()))
            {
                return Err(Box::new(std::io::Error::other("上游地址被安全策略拒绝"))
                    as Box<dyn std::error::Error + Send + Sync>);
            }
            Ok(Box::new(addresses.into_iter()) as Addrs)
        })
    }
}

fn is_restricted_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let octets = address.octets();
            address.is_private()
                || address.is_loopback()
                || address.is_link_local()
                || address.is_unspecified()
                || address.is_multicast()
                || address.is_broadcast()
                || matches!(octets, [100, second, ..] if second & 0xc0 == 0x40)
        }
        IpAddr::V6(address) => {
            address.is_loopback()
                || address.is_unique_local()
                || address.is_unicast_link_local()
                || address.is_unspecified()
                || address.is_multicast()
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|mapped| is_restricted_address(IpAddr::V4(mapped)))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_private_and_special_ip_literals() {
        for address in [
            "127.0.0.1",
            "10.0.0.1",
            "100.64.0.1",
            "169.254.169.254",
            "[::1]",
            "[fe80::1]",
            "[::ffff:127.0.0.1]",
        ] {
            assert!(matches!(
                validate_target(&format!("http://{address}"), false),
                Err(TargetError::Restricted)
            ));
        }
        assert!(validate_target("https://8.8.8.8", false).is_ok());
        assert!(validate_target("http://127.0.0.1", true).is_ok());
    }

    #[tokio::test]
    async fn public_client_rejects_private_dns_results() {
        let clients = OutboundClients::new();
        let result = clients
            .for_policy(false)
            .get("http://localhost:9")
            .send()
            .await;
        assert!(result.is_err(), "私网 DNS 结果不应进入连接阶段");
    }
}

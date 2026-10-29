use anyhow::{bail, Context, Result};
use reqwest::{
    blocking::{Client, Response},
    redirect::Policy,
    Url,
};
use std::{
    net::{IpAddr, ToSocketAddrs},
    time::Duration,
};

fn public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => {
            let [a, b, c, _] = ip.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && (b == 168 || b == 0 || (b == 88 && c == 99)))
                || (a == 198 && (b == 18 || b == 19))
                || ip.is_broadcast())
        }
        IpAddr::V6(ip) => {
            if let Some(v4) = ip.to_ipv4_mapped() {
                return public_ip(v4.into());
            }
            let s = ip.segments();
            !ip.is_loopback()
                && !ip.is_unspecified()
                && !ip.is_multicast()
                && (s[0] & 0xfe00 != 0xfc00)
                && (s[0] & 0xffc0 != 0xfe80)
                && (s[0] & 0xe000 == 0x2000)
        }
    }
}

pub fn get(mut url: Url) -> Result<Response> {
    for _ in 0..=8 {
        if !matches!(url.scheme(), "http" | "https")
            || !url.username().is_empty()
            || url.password().is_some()
        {
            bail!("지원하지 않는 원문 주소");
        }
        let host = url.host_str().context("원문 호스트가 없습니다")?.to_owned();
        let port = url.port_or_known_default().context("포트가 없습니다")?;
        if port != 80 && port != 443 {
            bail!("원문 주소의 포트가 허용되지 않습니다");
        }
        let addrs: Vec<_> = (host.as_str(), port).to_socket_addrs()?.collect();
        if addrs.is_empty() || addrs.iter().any(|a| !public_ip(a.ip())) {
            bail!("내부 네트워크 주소는 기사 본문으로 열 수 없습니다");
        }
        let client = Client::builder()
            .no_proxy()
            .redirect(Policy::none())
            .timeout(Duration::from_secs(25))
            .user_agent("Mozilla/5.0 NewsMoa/1.4")
            .resolve_to_addrs(&host, &addrs)
            .build()?;
        let response = client.get(url.clone()).send()?;
        if response.status().is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .context("이동할 주소가 없습니다")?
                .to_str()?;
            url = url.join(location)?;
            continue;
        }
        return Ok(response.error_for_status()?);
    }
    bail!("원문 주소 이동 횟수 초과")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn blocks_private_and_mapped_addresses() {
        for s in [
            "127.0.0.1",
            "192.168.1.1",
            "10.0.0.2",
            "169.254.169.254",
            "100.64.0.1",
            "::1",
            "::ffff:127.0.0.1",
            "fc00::1",
            "fe80::1",
        ] {
            assert!(!public_ip(s.parse().unwrap()), "{s}");
        }
        assert!(public_ip("8.8.8.8".parse().unwrap()));
        assert!(public_ip("2606:4700:4700::1111".parse().unwrap()));
        assert!(get(Url::parse("http://127.0.0.1/").unwrap()).is_err());
    }
}

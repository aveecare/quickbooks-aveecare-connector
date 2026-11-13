//! What QuickBooks says about itself (HostQuery): the edition and the qbXML
//! versions this file speaks. The Web Connector sends the same query before
//! anything else and hands the answer to the server; so do we.

use serde::Serialize;

pub fn host_query_xml(version: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"utf-8\"?>\n<?qbxml version=\"{version}\"?>\n<QBXML><QBXMLMsgsRq onError=\"stopOnError\"><HostQueryRq requestID=\"host\" /></QBXMLMsgsRq></QBXML>"
    )
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HostInfo {
    /// "QuickBooks Enterprise Solutions: Accountant 24.0".
    pub product_name: String,
    pub major_version: String,
    pub minor_version: String,
    pub country: String,
    /// The newest qbXML version the file supports, "17.0".
    pub qbxml_version: String,
}

fn tag<'a>(xml: &'a str, name: &str) -> Option<&'a str> {
    let open = format!("<{name}>");
    let close = format!("</{name}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].trim())
}

fn decode(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

fn version_key(v: &str) -> (u32, u32) {
    let mut parts = v.trim().split('.');
    let major = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    let minor = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    (major, minor)
}

pub fn parse_host(xml: &str) -> Option<HostInfo> {
    let ret = tag(xml, "HostRet")?;
    let mut versions = Vec::new();
    let mut rest = ret;
    while let Some(v) = tag(rest, "SupportedQBXMLVersion") {
        versions.push(v.to_string());
        let close = "</SupportedQBXMLVersion>";
        match rest.find(close) {
            Some(i) => rest = &rest[i + close.len()..],
            None => break,
        }
    }
    let best = versions.iter().max_by_key(|v| version_key(v)).cloned().unwrap_or_default();
    Some(HostInfo {
        product_name: decode(tag(ret, "ProductName").unwrap_or("")),
        major_version: tag(ret, "MajorVersion").unwrap_or("").to_string(),
        minor_version: tag(ret, "MinorVersion").unwrap_or("").to_string(),
        country: tag(ret, "Country").unwrap_or("US").to_string(),
        qbxml_version: best,
    })
}

/// "17.0" -> ("17", "0").
pub fn split_version(v: &str) -> (String, String) {
    let (major, minor) = version_key(v);
    (major.to_string(), minor.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ENTERPRISE: &str = r#"<?xml version="1.0" ?><QBXML><QBXMLMsgsRs><HostQueryRs requestID="host" statusCode="0" statusSeverity="Info" statusMessage="Status OK"><HostRet><ProductName>QuickBooks Enterprise Solutions: Accountant 24.0</ProductName><MajorVersion>34</MajorVersion><MinorVersion>0</MinorVersion><Country>US</Country><SupportedQBXMLVersion>1.0</SupportedQBXMLVersion><SupportedQBXMLVersion>13.0</SupportedQBXMLVersion><SupportedQBXMLVersion>16.0</SupportedQBXMLVersion><SupportedQBXMLVersion>17.0</SupportedQBXMLVersion><SupportedQBXMLVersion>2.1</SupportedQBXMLVersion><IsAutomaticLogin>false</IsAutomaticLogin><QBFileMode>SingleUser</QBFileMode></HostRet></HostQueryRs></QBXMLMsgsRs></QBXML>"#;

    #[test]
    fn reads_edition_and_newest_version() {
        let h = parse_host(ENTERPRISE).unwrap();
        assert_eq!(h.product_name, "QuickBooks Enterprise Solutions: Accountant 24.0");
        assert_eq!(h.qbxml_version, "17.0");
        assert_eq!(split_version(&h.qbxml_version), ("17".into(), "0".into()));
        assert_eq!(h.country, "US");
    }

    #[test]
    fn no_host_ret_is_none() {
        assert!(parse_host("<QBXML/>").is_none());
        assert!(host_query_xml("13.0").contains("<?qbxml version=\"13.0\"?>"));
    }
}

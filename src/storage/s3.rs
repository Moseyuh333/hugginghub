//! Minimal S3 client with AWS Signature V4. Works against AWS S3 and
//! S3-compatible services (MinIO, R2, B2, Wasabi, Ceph...).
//!
//! Env: HH_S3_BUCKET, HH_S3_ENDPOINT (default https://s3.<region>.amazonaws.com),
//! HH_S3_REGION (default us-east-1), HH_S3_ACCESS_KEY / HH_S3_SECRET_KEY
//! (fallback AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY), HH_S3_PREFIX,
//! HH_S3_PATH_STYLE (default true), HH_S3_REDIRECT (presigned downloads, default false).

use anyhow::{anyhow, bail, Context, Result};
use axum::body::Body;
use futures_util::TryStreamExt;
use hmac::{Hmac, Mac};
use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
use sha2::{Digest, Sha256};
use std::path::Path;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

const UNRESERVED: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'.').remove(b'_').remove(b'~');
const PART_SIZE: u64 = 64 * 1024 * 1024;
const SINGLE_PUT_MAX: u64 = 256 * 1024 * 1024;

pub struct S3 {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    access: String,
    secret: String,
    prefix: String,
    path_style: bool,
    pub redirect: bool,
    http: reqwest::Client,
}

fn enc(s: &str) -> String {
    utf8_percent_encode(s, UNRESERVED).to_string()
}

fn hmac(key: &[u8], data: &str) -> Vec<u8> {
    let mut m = Hmac::<Sha256>::new_from_slice(key).unwrap();
    m.update(data.as_bytes());
    m.finalize().into_bytes().to_vec()
}

fn sha_hex(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

impl S3 {
    pub fn from_env() -> Result<S3> {
        let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let region = env("HH_S3_REGION").unwrap_or_else(|| "us-east-1".into());
        let s3 = S3 {
            endpoint: env("HH_S3_ENDPOINT")
                .unwrap_or_else(|| format!("https://s3.{region}.amazonaws.com"))
                .trim_end_matches('/')
                .to_string(),
            bucket: env("HH_S3_BUCKET").context("HH_S3_BUCKET is required when HH_STORAGE=s3")?,
            access: env("HH_S3_ACCESS_KEY").or_else(|| env("AWS_ACCESS_KEY_ID")).context("HH_S3_ACCESS_KEY missing")?,
            secret: env("HH_S3_SECRET_KEY").or_else(|| env("AWS_SECRET_ACCESS_KEY")).context("HH_S3_SECRET_KEY missing")?,
            prefix: env("HH_S3_PREFIX").map(|p| format!("{}/", p.trim_matches('/'))).unwrap_or_default(),
            path_style: env("HH_S3_PATH_STYLE").map(|v| v != "0" && v != "false").unwrap_or(true),
            redirect: env("HH_S3_REDIRECT").map(|v| v == "1" || v == "true").unwrap_or(false),
            region,
            http: reqwest::Client::builder().build()?,
        };
        Ok(s3)
    }

    /// (host, canonical path) for an object key.
    fn locate(&self, key: &str) -> (String, String, String) {
        let full = format!("{}{}", self.prefix, key);
        let ekey: String = full.split('/').map(enc).collect::<Vec<_>>().join("/");
        let (scheme, host) = self.endpoint.split_once("://").unwrap_or(("https", &self.endpoint));
        if self.path_style {
            (scheme.to_string(), host.to_string(), format!("/{}/{}", self.bucket, ekey))
        } else {
            (scheme.to_string(), format!("{}.{}", self.bucket, host), format!("/{ekey}"))
        }
    }

    fn signing_key(&self, date: &str) -> Vec<u8> {
        let k = hmac(format!("AWS4{}", self.secret).as_bytes(), date);
        let k = hmac(&k, &self.region);
        let k = hmac(&k, "s3");
        hmac(&k, "aws4_request")
    }

    /// Sign a request; returns the headers to send (host + x-amz-* + authorization).
    fn sign(&self, method: &str, host: &str, path: &str, query: &[(String, String)], payload_hash: &str, extra: &[(&str, String)]) -> Vec<(String, String)> {
        let now = chrono::Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let mut q: Vec<(String, String)> = query.iter().map(|(k, v)| (enc(k), enc(v))).collect();
        q.sort();
        let cq = q.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
        let mut hdrs: Vec<(String, String)> = vec![
            ("host".into(), host.to_string()),
            ("x-amz-content-sha256".into(), payload_hash.to_string()),
            ("x-amz-date".into(), amz_date.clone()),
        ];
        for (k, v) in extra {
            hdrs.push((k.to_ascii_lowercase(), v.trim().to_string()));
        }
        hdrs.sort();
        let ch: String = hdrs.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
        let sh = hdrs.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(";");
        let creq = format!("{method}\n{path}\n{cq}\n{ch}\n{sh}\n{payload_hash}");
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let sts = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", sha_hex(creq.as_bytes()));
        let sig = hex::encode(hmac(&self.signing_key(&date), &sts));
        hdrs.push((
            "authorization".into(),
            format!("AWS4-HMAC-SHA256 Credential={}/{scope}, SignedHeaders={sh}, Signature={sig}", self.access),
        ));
        hdrs.retain(|(k, _)| k != "host");
        hdrs
    }

    fn url(scheme: &str, host: &str, path: &str, query: &[(String, String)]) -> String {
        let qs = query.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&");
        if qs.is_empty() { format!("{scheme}://{host}{path}") } else { format!("{scheme}://{host}{path}?{qs}") }
    }

    async fn send(&self, method: reqwest::Method, key: &str, query: Vec<(String, String)>, body: Option<Vec<u8>>, extra: Vec<(&str, String)>) -> Result<reqwest::Response> {
        let (scheme, host, path) = self.locate(key);
        let hash = match &body { Some(b) => sha_hex(b), None => sha_hex(b"") };
        let hdrs = self.sign(method.as_str(), &host, &path, &query, &hash, &extra);
        let mut req = self.http.request(method, Self::url(&scheme, &host, &path, &query));
        for (k, v) in hdrs {
            req = req.header(k, v);
        }
        if let Some(b) = body {
            req = req.body(b);
        }
        Ok(req.send().await?)
    }

    pub async fn head(&self, key: &str) -> Result<Option<u64>> {
        let r = self.send(reqwest::Method::HEAD, key, vec![], None, vec![]).await?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !r.status().is_success() {
            bail!("S3 HEAD {key}: {}", r.status());
        }
        Ok(r.headers().get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse().ok()))
    }

    pub async fn get_range(&self, key: &str, start: u64, len: u64) -> Result<Option<Body>> {
        if len == 0 {
            return Ok(Some(Body::empty()));
        }
        let range = format!("bytes={}-{}", start, start + len - 1);
        let r = self.send(reqwest::Method::GET, key, vec![], None, vec![("range", range)]).await?;
        if r.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        if !r.status().is_success() {
            bail!("S3 GET {key}: {}", r.status());
        }
        Ok(Some(Body::from_stream(r.bytes_stream().map_err(std::io::Error::other))))
    }

    async fn read_chunk(f: &mut tokio::fs::File, off: u64, len: u64) -> Result<Vec<u8>> {
        f.seek(std::io::SeekFrom::Start(off)).await?;
        let mut buf = vec![0u8; len as usize];
        f.read_exact(&mut buf).await?;
        Ok(buf)
    }

    pub async fn put_file(&self, key: &str, path: &Path, size: u64) -> Result<()> {
        let mut f = tokio::fs::File::open(path).await?;
        if size <= SINGLE_PUT_MAX {
            let data = Self::read_chunk(&mut f, 0, size).await?;
            let r = self.send(reqwest::Method::PUT, key, vec![], Some(data), vec![]).await?;
            if !r.status().is_success() {
                bail!("S3 PUT {key}: {} {}", r.status(), r.text().await.unwrap_or_default());
            }
            return Ok(());
        }
        // multipart upload
        let r = self.send(reqwest::Method::POST, key, vec![("uploads".into(), String::new())], Some(vec![]), vec![]).await?;
        let txt = r.text().await?;
        let upload_id = xml_tag(&txt, "UploadId").ok_or_else(|| anyhow!("S3 multipart init failed: {txt}"))?;
        let mut parts = vec![];
        let mut off = 0u64;
        let mut n = 1u32;
        let res: Result<()> = async {
            while off < size {
                let len = PART_SIZE.min(size - off);
                let data = Self::read_chunk(&mut f, off, len).await?;
                let q = vec![("partNumber".into(), n.to_string()), ("uploadId".into(), upload_id.clone())];
                let r = self.send(reqwest::Method::PUT, key, q, Some(data), vec![]).await?;
                if !r.status().is_success() {
                    bail!("S3 upload part {n}: {}", r.status());
                }
                let etag = r.headers().get("etag").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
                parts.push(format!("<Part><PartNumber>{n}</PartNumber><ETag>{etag}</ETag></Part>"));
                off += len;
                n += 1;
            }
            let xml = format!("<CompleteMultipartUpload>{}</CompleteMultipartUpload>", parts.join(""));
            let q = vec![("uploadId".into(), upload_id.clone())];
            let r = self.send(reqwest::Method::POST, key, q, Some(xml.into_bytes()), vec![]).await?;
            let st = r.status();
            let body = r.text().await.unwrap_or_default();
            if !st.is_success() || body.contains("<Error>") {
                bail!("S3 multipart complete: {st} {body}");
            }
            Ok(())
        }
        .await;
        if res.is_err() {
            let q = vec![("uploadId".into(), upload_id)];
            let _ = self.send(reqwest::Method::DELETE, key, q, None, vec![]).await;
        }
        res
    }

    /// Query-string presigned GET URL.
    pub fn presign_get(&self, key: &str, expires: u64, filename: Option<&str>) -> String {
        let (scheme, host, path) = self.locate(key);
        let now = chrono::Utc::now();
        let amz_date = now.format("%Y%m%dT%H%M%SZ").to_string();
        let date = now.format("%Y%m%d").to_string();
        let scope = format!("{date}/{}/s3/aws4_request", self.region);
        let mut q: Vec<(String, String)> = vec![
            ("X-Amz-Algorithm".into(), "AWS4-HMAC-SHA256".into()),
            ("X-Amz-Credential".into(), format!("{}/{scope}", self.access)),
            ("X-Amz-Date".into(), amz_date.clone()),
            ("X-Amz-Expires".into(), expires.to_string()),
            ("X-Amz-SignedHeaders".into(), "host".into()),
        ];
        if let Some(f) = filename {
            q.push(("response-content-disposition".into(), format!("attachment; filename=\"{}\"", f.replace('"', ""))));
        }
        let mut sq: Vec<(String, String)> = q.iter().map(|(k, v)| (enc(k), enc(v))).collect();
        sq.sort();
        let cq = sq.iter().map(|(k, v)| format!("{k}={v}")).collect::<Vec<_>>().join("&");
        let creq = format!("GET\n{path}\n{cq}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD");
        let sts = format!("AWS4-HMAC-SHA256\n{amz_date}\n{scope}\n{}", sha_hex(creq.as_bytes()));
        let sig = hex::encode(hmac(&self.signing_key(&date), &sts));
        format!("{scheme}://{host}{path}?{cq}&X-Amz-Signature={sig}")
    }
}

fn xml_tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let s = xml.find(&open)? + open.len();
    let e = xml[s..].find(&format!("</{tag}>"))? + s;
    Some(xml[s..e].to_string())
}

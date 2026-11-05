//! Git LFS pointer files and the on-disk LFS object store.

pub const POINTER_MAX: usize = 1024;

#[derive(Clone, Debug, PartialEq)]
pub struct Pointer {
    pub oid: String,
    pub size: i64,
}

pub fn pointer_text(oid: &str, size: i64) -> String {
    format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {size}\n")
}

pub fn parse_pointer(data: &[u8]) -> Option<Pointer> {
    if data.len() > POINTER_MAX {
        return None;
    }
    let s = std::str::from_utf8(data).ok()?;
    if !s.starts_with("version https://git-lfs.github.com/spec/") {
        return None;
    }
    let mut oid = None;
    let mut size = None;
    for line in s.lines() {
        if let Some(o) = line.strip_prefix("oid sha256:") {
            oid = Some(o.trim().to_string());
        } else if let Some(n) = line.strip_prefix("size ") {
            size = n.trim().parse().ok();
        }
    }
    let oid = oid?;
    if oid.len() != 64 || !oid.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(Pointer { oid: oid.to_ascii_lowercase(), size: size? })
}

pub fn valid_oid(oid: &str) -> bool {
    oid.len() == 64 && oid.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f'))
}

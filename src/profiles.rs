use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ImageProfileKind {
    #[default]
    TsRustPostgres,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImageProfileDefinition {
    pub kind: ImageProfileKind,
    pub name: &'static str,
    pub description: &'static str,
    pub default_vcpus: u8,
    pub default_memory_mb: u32,
    pub disk_mb: u32,
}

#[derive(Debug, Clone)]
pub struct ResolvedImageProfile {
    pub base_rootfs_path: PathBuf,
}

pub fn list_profiles() -> Vec<ImageProfileDefinition> {
    vec![ImageProfileDefinition {
        kind: ImageProfileKind::TsRustPostgres,
        name: "ts-rust-postgres",
        description: "TypeScript, Rust, PostgreSQL, Node, pnpm, just, and common dev tooling",
        default_vcpus: 2,
        default_memory_mb: 4096,
        disk_mb: 50 * 1024,
    }]
}

pub fn resolve_profile(data_dir: &Path, kind: ImageProfileKind) -> ResolvedImageProfile {
    let base_rootfs_path = match kind {
        ImageProfileKind::TsRustPostgres => data_dir.join("base.ext4"),
    };

    ResolvedImageProfile { base_rootfs_path }
}

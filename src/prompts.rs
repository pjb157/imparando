use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct PromptReference {
    pub id: &'static str,
    pub title: &'static str,
    pub summary: &'static str,
    pub vm_path: &'static str,
    pub body: &'static str,
}

const GITHUB_AUTH_BODY: &str = include_str!("../prompts/github-auth.md");
const POSTGRES_START_BODY: &str = include_str!("../prompts/postgres-start.md");

pub fn built_in_prompts() -> Vec<PromptReference> {
    vec![
        PromptReference {
            id: "github-auth",
            title: "GitHub Auth",
            summary: "Explains how git auth works in Imparando VMs and how agents should push safely.",
            vm_path: "/root/.imparando/prompts/github-auth.md",
            body: GITHUB_AUTH_BODY,
        },
        PromptReference {
            id: "postgres-start",
            title: "Postgres Recovery",
            summary: "Explains how to check and start local PostgreSQL in a VM when agents cannot reach 127.0.0.1:5432.",
            vm_path: "/root/.imparando/prompts/postgres-start.md",
            body: POSTGRES_START_BODY,
        },
    ]
}

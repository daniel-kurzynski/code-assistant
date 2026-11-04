"""The demo environment: two small projects and the app's configuration.

Everything the screenshots show comes from here and from screenshots.yaml, so
a run starts from the same state every time.
"""

import json
import os
import subprocess

API_CLIENT = {
    ".gitignore": "target/\n",
    "Cargo.toml": """\
[package]
name = "api-client"
version = "0.4.2"
edition = "2021"

[dependencies]
""",
    "README.md": """\
# api-client

A small HTTP API client with retries and exponential backoff.
""",
    "src/lib.rs": """\
//! A small HTTP API client with retries.

pub mod client;
""",
    "src/client/mod.rs": """\
pub mod backoff;
pub mod retry;
""",
    "src/client/backoff.rs": """\
use std::time::Duration;

/// Exponential backoff: each attempt waits twice as long as the one before.
pub struct Backoff {
    base: Duration,
}

impl Backoff {
    pub fn new(base: Duration) -> Self {
        Self { base }
    }

    pub fn next_delay(&mut self, attempt: u32) -> Duration {
        self.base * 2u32.pow(attempt.saturating_sub(1))
    }
}
""",
    "src/client/retry.rs": """\
use std::thread::sleep;
use std::time::{Duration, Instant};

use super::backoff::Backoff;

/// Milliseconds to wait when the server sent no `Retry-After` header.
fn retry_after_jitter() -> u64 {
    let nanos = Instant::now().elapsed().subsec_nanos() as u64;
    nanos % 40
}

/// Run `op` until it succeeds or `max_attempts` are used up.
pub fn with_retry<T, E>(max_attempts: u32, mut op: impl FnMut(u32) -> Result<T, E>) -> Result<T, E> {
    let mut backoff = Backoff::new(Duration::from_millis(10));
    let mut attempt = 0;
    loop {
        attempt += 1;
        match op(attempt) {
            Ok(value) => return Ok(value),
            Err(err) if attempt >= max_attempts => return Err(err),
            Err(_) => {
                let jitter = retry_after_jitter();
                sleep(Duration::from_millis(jitter));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_with_backoff() {
        let start = Instant::now();
        let result: Result<(), ()> = with_retry(3, |_| Err(()));
        assert!(result.is_err());
        assert!(start.elapsed() >= Duration::from_millis(30));
    }

    #[test]
    fn returns_first_success() {
        let result: Result<u32, ()> = with_retry(5, |attempt| if attempt == 2 { Ok(attempt) } else { Err(()) });
        assert_eq!(result, Ok(2));
    }
}
""",
}

WEB_SHOP = {
    ".gitignore": "node_modules/\ndist/\n",
    "package.json": """\
{
  "name": "web-shop",
  "version": "2.3.0",
  "private": true,
  "scripts": { "dev": "vite", "build": "vite build", "test": "vitest run" },
  "dependencies": { "react": "^18.2.0", "react-dom": "^18.2.0", "zustand": "^4.4.1" },
  "devDependencies": { "vite": "^5.0.8", "vitest": "^1.1.0", "typescript": "^5.3.3" }
}
""",
    "src/components/Sidebar.tsx": """\
import { useCart } from "../store";

export function Sidebar({ categories }: { categories: string[] }) {
  const items = useCart((s) => s.items);
  return (
    <aside className="sidebar">
      {categories.map((c) => (
        <a key={c} href={`/c/${c}`}>{c} ({items.filter((i) => i.category === c).length})</a>
      ))}
    </aside>
  );
}
""",
    "src/store.ts": """\
import { create } from "zustand";

export const useCart = create<{ items: { id: string; category: string }[] }>(() => ({ items: [] }));
""",
}

GIT_ENV = {"GIT_AUTHOR_NAME": "Demo", "GIT_AUTHOR_EMAIL": "demo@example.com",
           "GIT_COMMITTER_NAME": "Demo", "GIT_COMMITTER_EMAIL": "demo@example.com",
           "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1"}


def git(cwd, *args):
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True,
                   env={**os.environ, **GIT_ENV})


def write_tree(root, files):
    for rel, content in files.items():
        path = os.path.join(root, rel)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w") as f:
            f.write(content)


def write_project(root, files, message):
    write_tree(root, files)
    git(root, "init", "-q", "-b", "main")
    git(root, "add", "-A")
    git(root, "commit", "-q", "-m", message)


def write_json(path, obj):
    with open(path, "w") as f:
        json.dump(obj, f, indent=2)


def create(work, port, here, width, height):
    """Create projects and config below `work`; returns the paths."""
    projects = os.path.join(work, "projects")
    paths = {
        "api-client": os.path.join(projects, "api-client"),
        "web-shop": os.path.join(projects, "web-shop"),
        "config": os.path.join(work, "config"),
        "data": os.path.join(work, "data"),
    }
    write_project(paths["api-client"], API_CLIENT, "Initial client")
    write_project(paths["web-shop"], WEB_SHOP, "Shop skeleton")
    # An existing worktree for the worktree menu.
    git(paths["web-shop"], "worktree", "add", "-q", "-b", "feature/search",
        os.path.join(projects, "web-shop-search"))

    cfg = paths["config"]
    os.makedirs(cfg)
    os.makedirs(paths["data"])
    write_json(os.path.join(cfg, "providers.json"), {
        "anthropic": {"label": "Anthropic", "provider": "anthropic",
                      "config": {"api_key": "demo", "base_url": f"http://127.0.0.1:{port}/v1"}}})
    write_json(os.path.join(cfg, "model-selection.json"), {"models": [
        {"provider": "anthropic", "id": "claude-sonnet-5-5", "default_effort": "medium"},
        {"provider": "anthropic", "id": "claude-opus-5-5"},
        {"provider": "anthropic", "id": "claude-haiku-5-5"}]})
    write_json(os.path.join(cfg, "default-model.json"), {"model": "anthropic/claude-sonnet-5-5"})
    write_json(os.path.join(cfg, "projects.json"), {
        "api-client": {"path": paths["api-client"]}, "web-shop": {"path": paths["web-shop"]}})
    write_json(os.path.join(cfg, "mcp-servers.json"), {"servers": {
        "jira": {"command": "python3", "args": [os.path.join(here, "fake_mcp.py"), "jira"]},
        # Answers with an OAuth challenge: shows as "Log in" in the Tools menu.
        "docs": {"url": f"http://127.0.0.1:{port}/mcp"}}})
    write_json(os.path.join(cfg, "ui-settings.json"), {
        "theme_mode": "light", "ui_scale": 1.0,
        "window_bounds": {"x": 0, "y": 0, "width": width, "height": height},
        "right_sidebar_width": 330,
        "review_expanded_repos": [paths["api-client"]],
        "sidebar": {"view": "projects", "project_order": ["api-client", "web-shop"]}})
    return paths

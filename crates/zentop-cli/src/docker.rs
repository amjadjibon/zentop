//! Read container state from the guest's local Docker socket with a fixed query.
use smol::{future::race, io::AsyncReadExt, process::Command, Timer};
use std::{process::Stdio, time::Duration};
use zentop_core::ssh_stream::{self, Container, ContainerHealth, ContainerState, Docker};

pub fn collect() -> Docker {
    smol::block_on(race(query(), async {
        Timer::after(Duration::from_secs(2)).await;
        Docker::Unavailable
    }))
}

async fn query() -> Docker {
    let Ok(mut child) = Command::new("docker")
        .env_remove("DOCKER_CONTEXT")
        .args([
            "--host",
            "unix:///var/run/docker.sock",
            "container",
            "ls",
            "--all",
            "--last",
            &(ssh_stream::MAX_CONTAINERS + 1).to_string(),
            "--format",
            "{{json .Names}}\t{{json .State}}\t{{json .HealthStatus}}",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .reap_on_drop(true)
        .spawn()
    else {
        return Docker::Unavailable;
    };
    let Some(stdout) = child.stdout.take() else {
        return Docker::Unavailable;
    };
    let mut bytes = Vec::new();
    if stdout
        .take(32 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await
        .is_err()
        || bytes.len() > 32 * 1024
    {
        return Docker::Unavailable;
    }
    if !child.status().await.is_ok_and(|status| status.success()) {
        return Docker::Unavailable;
    }
    parse(&bytes).unwrap_or(Docker::Unavailable)
}

fn parse(bytes: &[u8]) -> Option<Docker> {
    let mut containers = Vec::new();
    for line in std::str::from_utf8(bytes).ok()?.lines() {
        let (name, tail) = line.split_once('\t')?;
        let (state, health) = tail.split_once('\t')?;
        let name: String = serde_json::from_str(name).ok()?;
        let state: String = serde_json::from_str(state).ok()?;
        let health: String = serde_json::from_str(health).ok()?;
        containers.push(Container {
            name: ssh_stream::safe_display(&name, 64),
            state: match state.as_str() {
                "running" => ContainerState::Running,
                "exited" => ContainerState::Exited,
                "created" => ContainerState::Created,
                "paused" => ContainerState::Paused,
                "restarting" => ContainerState::Restarting,
                "removing" => ContainerState::Removing,
                "dead" => ContainerState::Dead,
                _ => ContainerState::Unknown,
            },
            health: match health.as_str() {
                "starting" => Some(ContainerHealth::Starting),
                "healthy" => Some(ContainerHealth::Healthy),
                "unhealthy" => Some(ContainerHealth::Unhealthy),
                _ => None,
            },
        });
    }
    let truncated = containers.len() > ssh_stream::MAX_CONTAINERS;
    containers.truncate(ssh_stream::MAX_CONTAINERS);
    Some(Docker::Available {
        containers,
        truncated,
    })
}
